//! Negotiation layer v1: arrival-time contract menus.
//!
//! For each session, the building offers a menu of contracts. Tier 0 is the
//! original request; deeper tiers trade a later departure and/or a reduced
//! target for a lower price; the final slot is always a REJECT option (charge
//! elsewhere at an external price, original terms kept). A seeded softmax
//! choice model picks one offer per session; chosen deviations are written
//! back into a modified scenario that then runs through the normal engine.
//!
//! Pricing: each tier is priced from the building's cost to serve that
//! contract, computed by a single-session LP over the session's window
//! (reusing the oracle solver on a one-vehicle sub-scenario). The building
//! passes `surplus_share` of the cost saving of a deviation on to the user:
//! `price_t = cost_0 - surplus_share * (cost_0 - cost_t)`, floored at zero.
//!
//! v1 approximations (documented, revisit in v2):
//! - Menus are priced session-alone against the building load; other EVs'
//!   concurrent draw is not in the pricing LP (it is in the actual dispatch).
//! - Under persistence, the arrival SoC used for pricing session k+1 assumes
//!   session k departs exactly at its (possibly renegotiated) target.
//! - Negotiation is a PRE-PASS over arrivals in time order, not an in-loop
//!   hook; the engine then runs once on the modified scenario.

use crate::milp::{MilpBackend, SolveError};
use crate::policy::oracle::{solve_oracle, OracleConfig};
use crate::scenario::Scenario;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct NegotiationConfig {
    /// Menu size INCLUDING the reject option (>= 2).
    pub choice_count: usize,
    /// Departure delay added per tier, slots.
    pub delay_slots_per_tier: usize,
    /// Target reduction per tier, kWh.
    pub target_reduction_per_tier_kwh: f64,
    /// Share of the building's cost saving passed to the user in [0, 1].
    pub surplus_share: f64,
    /// Price of charging elsewhere (reject option), USD/kWh of the deficit.
    pub external_price_usd_per_kwh: f64,
    /// Inconvenience cost per slot of departure delay, USD (choice model).
    pub inconvenience_delay_usd_per_slot: f64,
    /// Inconvenience cost per kWh of target reduction, USD (choice model).
    pub inconvenience_reduction_usd_per_kwh: f64,
    /// Softmax temperature; 0 = deterministic argmax.
    pub temperature: f64,
    /// RNG seed for the choice model (deterministic given the seed).
    pub seed: u64,
}

impl Default for NegotiationConfig {
    fn default() -> Self {
        NegotiationConfig {
            choice_count: 5,
            delay_slots_per_tier: 4,
            target_reduction_per_tier_kwh: 2.0,
            surplus_share: 0.5,
            external_price_usd_per_kwh: 0.45,
            inconvenience_delay_usd_per_slot: 0.10,
            inconvenience_reduction_usd_per_kwh: 0.25,
            temperature: 0.0,
            seed: 42,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Offer {
    pub tier: usize,
    pub is_reject: bool,
    pub delay_slots: usize,
    pub target_reduction_kwh: f64,
    pub price_usd: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContractRecord {
    pub vehicle_id: u32,
    pub arrival_slot: usize,
    pub offers: Vec<Offer>,
    pub utilities: Vec<f64>,
    pub chosen_tier: usize,
    pub chosen_is_reject: bool,
    pub new_departure_slot: usize,
    pub new_target_kwh: f64,
}

/// Deterministic xorshift64* (same generator family as the test sweep).
struct Rng(u64);

impl Rng {
    fn f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Run the negotiation pre-pass. Returns the modified scenario (renegotiated
/// departures/targets) and one contract record per session, in arrival order.
pub fn negotiate(
    scenario: &Scenario,
    backend: &dyn MilpBackend,
    config: &NegotiationConfig,
) -> Result<(Scenario, Vec<ContractRecord>), SolveError> {
    assert!(
        config.choice_count >= 2,
        "menu needs at least one offer plus reject"
    );
    let mut rng = Rng(config.seed | 1);
    let mut modified = scenario.clone();
    let mut records = Vec::new();

    // Arrival order over row indices.
    let mut order: Vec<usize> = (0..scenario.vehicles.len()).collect();
    order.sort_by_key(|&i| {
        (
            scenario.vehicles[i].arrival_slot,
            scenario.vehicles[i].vehicle_id,
        )
    });

    // Next session's arrival per row (same vehicle), to cap departure delays.
    let mut next_arrival: HashMap<usize, usize> = HashMap::new();
    for w in order.windows(2) {
        if scenario.vehicles[w[0]].vehicle_id == scenario.vehicles[w[1]].vehicle_id {
            next_arrival.insert(w[0], scenario.vehicles[w[1]].arrival_slot);
        }
    }

    // Chain estimate: departure SoC assumed equal to the (renegotiated)
    // target (v1 approximation).
    let mut chain_soc: HashMap<u32, f64> = HashMap::new();

    for &i in &order {
        let v = &scenario.vehicles[i];
        let arrival_soc = if modified.manifest.persistence {
            chain_soc
                .get(&v.vehicle_id)
                .map(|&prev| (prev - v.depletion_kwh).clamp(v.min_soc_kwh, v.battery_kwh))
                .unwrap_or(v.soc_arrival_kwh)
        } else {
            v.soc_arrival_kwh
        };

        // Cost to serve a contract: single-session LP on a sub-scenario.
        let max_dep = next_arrival
            .get(&i)
            .copied()
            .unwrap_or(scenario.manifest.horizon_slots)
            .min(scenario.manifest.horizon_slots);
        let cost_of = |dep: usize, target: f64| -> Result<f64, SolveError> {
            let mut sub = scenario.clone();
            let mut sv = scenario.vehicles[i].clone();
            sv.soc_arrival_kwh = arrival_soc;
            sv.departure_slot = dep;
            sv.soc_target_kwh = target;
            sv.depletion_kwh = 0.0;
            sub.vehicles = vec![sv];
            // A single capability-compatible port for the pricing LP.
            let port = scenario
                .chargers
                .iter()
                .find(|c| c.bidirectional == (v.max_discharge_kw > 0.0))
                .or(scenario.chargers.first())
                .expect("validated scenario has chargers")
                .clone();
            sub.chargers = vec![port];
            let plan = solve_oracle(&sub, backend, &OracleConfig::default())?;
            Ok(plan.objective)
        };

        let cost_0 = cost_of(v.departure_slot, v.soc_target_kwh)?;
        let deficit_kwh = (v.soc_target_kwh - arrival_soc).max(0.0);

        let mut offers = Vec::new();
        for tier in 0..config.choice_count - 1 {
            let delay = tier * config.delay_slots_per_tier;
            let dep = (v.departure_slot + delay).min(max_dep);
            let reduction = (tier as f64 * config.target_reduction_per_tier_kwh)
                .min((v.soc_target_kwh - v.min_soc_kwh).max(0.0));
            let target = v.soc_target_kwh - reduction;
            let cost_t = cost_of(dep, target)?;
            let price = (cost_0 - config.surplus_share * (cost_0 - cost_t)).max(0.0);
            offers.push(Offer {
                tier,
                is_reject: false,
                delay_slots: dep - v.departure_slot,
                target_reduction_kwh: reduction,
                price_usd: price,
            });
        }
        offers.push(Offer {
            tier: config.choice_count - 1,
            is_reject: true,
            delay_slots: 0,
            target_reduction_kwh: 0.0,
            price_usd: config.external_price_usd_per_kwh * deficit_kwh,
        });

        // Choice model: utility = -price - inconvenience (reject exempt from
        // the inconvenience penalty).
        let utilities: Vec<f64> = offers
            .iter()
            .map(|o| {
                let inconvenience = if o.is_reject {
                    0.0
                } else {
                    config.inconvenience_delay_usd_per_slot * o.delay_slots as f64
                        + config.inconvenience_reduction_usd_per_kwh * o.target_reduction_kwh
                };
                -o.price_usd - inconvenience
            })
            .collect();
        let chosen = choose(&utilities, config.temperature, &mut rng);

        let offer = &offers[chosen];
        let (new_dep, new_target) = if offer.is_reject {
            (v.departure_slot, v.soc_target_kwh)
        } else {
            (
                v.departure_slot + offer.delay_slots,
                v.soc_target_kwh - offer.target_reduction_kwh,
            )
        };
        if !offer.is_reject {
            modified.vehicles[i].departure_slot = new_dep;
            modified.vehicles[i].soc_target_kwh = new_target;
        }
        chain_soc.insert(v.vehicle_id, new_target.max(arrival_soc.min(v.battery_kwh)));

        records.push(ContractRecord {
            vehicle_id: v.vehicle_id,
            arrival_slot: v.arrival_slot,
            chosen_tier: chosen,
            chosen_is_reject: offer.is_reject,
            new_departure_slot: new_dep,
            new_target_kwh: new_target,
            offers,
            utilities,
        });
    }

    modified
        .validate()
        .map_err(|e| SolveError::Backend(format!("negotiated scenario invalid: {e}")))?;
    Ok((modified, records))
}

/// Softmax sample over utilities (temperature > 0) or argmax (temperature 0).
/// Ties break toward the lower tier, deterministically.
fn choose(utilities: &[f64], temperature: f64, rng: &mut Rng) -> usize {
    if temperature <= 0.0 {
        let mut best = 0;
        for (k, &u) in utilities.iter().enumerate() {
            if u > utilities[best] {
                best = k;
            }
        }
        return best;
    }
    let max_u = utilities.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = utilities
        .iter()
        .map(|&u| ((u - max_u) / temperature).exp())
        .collect();
    let total: f64 = weights.iter().sum();
    let mut draw = rng.f64() * total;
    for (k, w) in weights.iter().enumerate() {
        draw -= w;
        if draw <= 0.0 {
            return k;
        }
    }
    weights.len() - 1
}
