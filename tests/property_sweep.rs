//! P19: randomized property sweep. A seeded xorshift generator produces 200
//! scenarios spanning contention, DR, persistence chains, efficiencies, and
//! site caps; every physical/billing invariant is asserted on every run of
//! every policy. Coverage counters prove the corpus actually exercises the
//! interesting regimes (a generator emitting trivial scenarios fails the
//! coverage assertions, not just vacuously passes).

mod common;

use common::{charger, dr_event, manifest, vehicle};
use openv2b::engine::{run, Results};
use openv2b::policy::{self, POLICY_NAMES};
use openv2b::scenario::{Scenario, TouClass};

/// Deterministic xorshift64* PRNG: no external dependency, fixed seed.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform float in [0, 1).
    fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f64()
    }
    fn usize(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next_u64() as usize) % (hi - lo + 1)
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }
}

fn random_scenario(rng: &mut Rng) -> Scenario {
    let horizon = rng.usize(24, 192);
    let n_chargers = rng.usize(1, 4);
    let chargers = (0..n_chargers)
        .map(|c| {
            let mut ch = charger(c as u32, rng.chance(0.5));
            ch.max_kw = rng.range(5.0, 25.0);
            ch
        })
        .collect();

    let n_vehicle_ids = rng.usize(1, 5);
    let mut vehicles = Vec::new();
    for id in 0..n_vehicle_ids {
        // Physical attributes are per-vehicle (validation requires sessions
        // of one vehicle to agree on them); session fields vary freely.
        let battery = rng.range(20.0, 100.0);
        let floor = rng.range(0.0, battery * 0.3);
        let max_charge_kw = rng.range(3.0, 22.0);
        let max_discharge_kw = if rng.chance(0.6) {
            rng.range(3.0, 22.0)
        } else {
            0.0
        };
        let n_sessions = rng.usize(1, 3);
        let mut cursor = rng.usize(0, horizon / 2);
        for _ in 0..n_sessions {
            if cursor + 2 >= horizon {
                break;
            }
            let dep = rng.usize(cursor + 2, horizon.min(cursor + 60));
            let mut v = vehicle(id as u32, cursor, dep);
            v.battery_kwh = battery;
            v.min_soc_kwh = floor;
            v.max_charge_kw = max_charge_kw;
            v.max_discharge_kw = max_discharge_kw;
            v.soc_arrival_kwh = rng.range(floor, battery);
            v.soc_target_kwh = rng.range(0.0, battery);
            v.depletion_kwh = rng.range(0.0, battery * 0.6);
            vehicles.push(v);
            cursor = dep + rng.usize(0, 20);
        }
    }

    let mut scenario = Scenario {
        manifest: manifest(horizon),
        vehicles,
        chargers,
        building_load_kw: (0..horizon).map(|_| rng.range(0.0, 80.0)).collect(),
        price_usd_per_kwh: (0..horizon).map(|_| rng.range(0.05, 0.50)).collect(),
        tou_class: (0..horizon)
            .map(|_| match rng.usize(0, 2) {
                0 => TouClass::Peak,
                1 => TouClass::OffPeak,
                _ => TouClass::SuperOffPeak,
            })
            .collect(),
        dr_events: Vec::new(),
    };
    scenario.manifest.charge_efficiency = rng.range(0.8, 1.0);
    scenario.manifest.discharge_efficiency = rng.range(0.8, 1.0);
    // Price the demand components so peak tracking is exercised (audit: an
    // unpriced sweep never notices a broken demand charge).
    scenario.manifest.demand_charge_usd_per_kw = rng.range(0.0, 5.0);
    scenario.manifest.demand_charge_peak_usd_per_kw = rng.range(0.0, 15.0);
    if rng.chance(0.5) {
        scenario.manifest.site_cap_kw = Some(rng.range(20.0, 90.0));
    }
    if rng.chance(0.3) {
        scenario.manifest.persistence = false;
    }
    // Up to two disjoint DR windows.
    let mut cursor = 0usize;
    for _ in 0..rng.usize(0, 2) {
        if cursor + 3 >= horizon - 1 {
            break;
        }
        let start = rng.usize(cursor, horizon - 3);
        let end = rng.usize(start + 1, (horizon - 1).min(start + 24));
        let mut e = dr_event(start, end, rng.range(10.0, 70.0));
        e.baseline_kw = rng.range(e.fsl_kw, 100.0);
        scenario.dr_events.push(e);
        cursor = end + 1;
    }
    scenario
}

/// All invariants asserted on one run. Returns coverage flags.
fn assert_invariants(scenario: &Scenario, r: &Results, label: &str) -> [bool; 6] {
    let dt = scenario.manifest.slot_minutes / 60.0;
    let eta_c = scenario.manifest.charge_efficiency;
    let eta_d = scenario.manifest.discharge_efficiency;
    let cap = scenario.manifest.site_cap_kw;
    let mut coverage = [false; 6];

    let max_fleet_kw: f64 = scenario.chargers.iter().map(|c| c.max_kw).sum();
    for rec in &r.slots {
        assert!(
            rec.ev_charge_kw >= -1e-9 && rec.ev_discharge_kw >= -1e-9,
            "{label}: sign"
        );
        assert!(
            rec.ev_charge_kw <= max_fleet_kw + 1e-9,
            "{label}: fleet charge cap"
        );
        assert!(
            rec.ev_discharge_kw <= max_fleet_kw + 1e-9,
            "{label}: fleet discharge cap"
        );
        assert!(rec.net_kw >= -1e-9, "{label}: export at slot {}", rec.slot);
        let bound = (rec.net_kw - rec.building_kw).abs();
        assert!(bound <= max_fleet_kw + 1e-9, "{label}: net vs building");
        if let Some(c) = cap {
            assert!(
                rec.net_kw <= rec.building_kw.max(c) + 1e-9,
                "{label}: site cap violated at slot {}",
                rec.slot
            );
            if rec.ev_charge_kw > 1e-6 && (rec.net_kw - c).abs() < 1e-6 {
                coverage[2] = true; // binding site cap
            }
        }
        if rec.ev_discharge_kw > 1e-6 {
            coverage[0] = true; // discharge exercised
        }
    }

    for s in &r.sessions {
        let v = scenario
            .vehicles
            .iter()
            .find(|v| v.vehicle_id == s.vehicle_id && v.arrival_slot == s.arrival_slot)
            .expect("session maps to a vehicle row");
        let expected =
            s.soc_arrival_kwh + eta_c * s.energy_drawn_kwh - s.energy_exported_kwh / eta_d;
        assert!(
            (s.soc_departure_kwh - expected).abs() < 1e-6,
            "{label}: conservation broken for vehicle {} ({} vs {})",
            s.vehicle_id,
            s.soc_departure_kwh,
            expected
        );
        assert!(
            s.soc_departure_kwh <= v.battery_kwh + 1e-6 && s.soc_departure_kwh >= -1e-6,
            "{label}: SoC out of [0, capacity]"
        );
        if s.never_connected {
            coverage[1] = true;
            assert!(
                (s.soc_departure_kwh - (s.soc_arrival_kwh)).abs() < 1e-9,
                "{label}: unserved session changed SoC"
            );
        }
        if s.missing_kwh > 1e-6 {
            coverage[3] = true; // target miss occurred somewhere
        }
        if s.chain_clamped_kwh > 1e-6 {
            coverage[4] = true; // infeasible-trip clamp exercised
        }
        // Session-outcome consistency (audit: these fields were previously
        // reported but never asserted in the sweep).
        assert!(
            (s.missing_kwh - (v.soc_target_kwh - s.soc_departure_kwh).max(0.0)).abs() < 1e-9,
            "{label}: missing_kwh definition"
        );
        assert!(
            (s.banked_kwh - (s.soc_departure_kwh - v.soc_target_kwh).max(0.0)).abs() < 1e-9,
            "{label}: banked_kwh definition"
        );
        assert_eq!(
            s.target_met,
            s.soc_departure_kwh + 1e-9 >= v.soc_target_kwh,
            "{label}: target_met flag inconsistent"
        );
    }

    // Per-port power caps via the trace (audit: fleet-sum bounds can hide a
    // per-port violation under a heterogeneous fleet).
    for t in &r.trace {
        let port = scenario
            .chargers
            .iter()
            .find(|c| c.charger_id == t.charger_id)
            .expect("trace names a real charger");
        assert!(
            t.power_kw.abs() <= port.max_kw + 1e-9,
            "{label}: per-port cap violated on charger {} at slot {}",
            t.charger_id,
            t.slot
        );
        if t.power_kw < 0.0 {
            assert!(
                port.bidirectional,
                "{label}: discharge on unidirectional charger {}",
                t.charger_id
            );
        }
    }

    // Bill identity, demand components, overflow and incentive definitions.
    let b = &r.bill;
    assert!(
        (b.total_usd - (b.energy_usd + b.demand_usd + b.dr_penalty_usd - b.dr_incentive_usd)).abs()
            < 1e-6,
        "{label}: bill identity"
    );
    let peak = r.slots.iter().map(|rec| rec.net_kw).fold(0.0f64, f64::max);
    let peak_tou = r
        .slots
        .iter()
        .filter(|rec| scenario.tou_class[rec.slot] == TouClass::Peak)
        .map(|rec| rec.net_kw)
        .fold(0.0f64, f64::max);
    assert!(
        (b.peak_net_kw - peak).abs() < 1e-9,
        "{label}: peak tracking"
    );
    assert!(
        (b.peak_net_peak_tou_kw - peak_tou).abs() < 1e-9,
        "{label}: peak-TOU tracking"
    );
    assert!(
        (b.demand_facilities_usd - scenario.manifest.demand_charge_usd_per_kw * peak).abs() < 1e-6,
        "{label}: facilities demand"
    );
    assert!(
        (b.demand_peak_tou_usd - scenario.manifest.demand_charge_peak_usd_per_kw * peak_tou).abs()
            < 1e-6,
        "{label}: peak-TOU demand"
    );
    let mut expected_incentive = 0.0;
    for (event, settlement) in scenario.dr_events.iter().zip(&b.dr_settlements) {
        let covered: Vec<&_> = r
            .slots
            .iter()
            .filter(|rec| rec.slot > event.start_slot && rec.slot <= event.end_slot)
            .collect();
        let expected: f64 = covered
            .iter()
            .map(|rec| (rec.net_kw - event.fsl_kw).max(0.0) * dt)
            .sum();
        assert!(
            (settlement.overflow_kwh - expected).abs() < 1e-6,
            "{label}: overflow definition"
        );
        if settlement.overflow_kwh > 1e-6 {
            coverage[5] = true; // DR overflow exercised
        }
        if !covered.is_empty() && expected <= 1e-9 {
            expected_incentive +=
                event.incentive_usd_per_kw * (event.baseline_kw - event.fsl_kw).max(0.0);
        }
    }
    assert!(
        (b.dr_incentive_usd - expected_incentive).abs() < 1e-6,
        "{label}: incentive rule (reduction-only, honored-gated)"
    );
    coverage
}

#[test]
fn randomized_scenarios_uphold_all_invariants() {
    let mut rng = Rng(0x5EED_2026_0730_0001);
    let mut coverage = [0usize; 6];
    let n = 200;
    for i in 0..n {
        let scenario = random_scenario(&mut rng);
        scenario
            .validate()
            .unwrap_or_else(|e| panic!("scenario {i} invalid: {e}"));
        for name in POLICY_NAMES {
            let pol = policy::by_name(name).expect("registered");
            let r = run(&scenario, pol.as_ref());
            let flags = assert_invariants(&scenario, &r, &format!("scenario {i} policy {name}"));
            for (c, f) in coverage.iter_mut().zip(flags) {
                *c += f as usize;
            }
        }
    }
    // The corpus must actually exercise the interesting regimes.
    let names = [
        "V2B discharge",
        "never_connected",
        "binding site cap",
        "target miss",
        "chain clamp",
        "DR overflow",
    ];
    for (count, name) in coverage.iter().zip(names) {
        assert!(
            *count >= 5,
            "coverage too thin: '{name}' hit only {count} times across {n} scenarios"
        );
    }
}
