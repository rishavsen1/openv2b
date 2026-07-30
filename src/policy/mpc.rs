//! Receding-horizon MPC policy over the solver-agnostic MILP layer.
//!
//! Honest information set: the model sees only currently-connected sessions
//! plus the public series (prices, building load, TOU classes, DR windows,
//! site cap). No knowledge of future arrivals. The engine still clamps every
//! setpoint, so a wrong solve can cost money but never break physics.
//!
//! The formulation is a pure LP (charge/discharge split into non-negative
//! variables): per connected session v and slot s in its remaining window,
//! grid-side charge energy cp[v,s] and building-side discharge energy
//! cn[v,s] (kWh/slot); SoC recursion e[v,s] = e[v,s-1] + eta_c*cp - cn/eta_d
//! within [floor, capacity]; a reachability terminal condition (the target
//! must still be attainable at full power after the modeled horizon, with a
//! high-penalty shortfall slack); aggregate net load agg[s] >= 0 (no-export)
//! and <= max(building, site cap); peak variables for both demand components;
//! DR overflow variables ov[s] >= (agg[s] - F) * dt priced at each event's
//! penalty rate; a small degradation cost on discharge so V2B is never free.

use crate::milp::{MilpBackend, Model, Sense, SolStatus, VarId};
use crate::policy::Policy;
use crate::state::{Observation, Setpoint};

pub struct MpcConfig {
    /// Maximum number of future slots modeled per solve.
    pub lookahead_slots: usize,
    /// Penalty per kWh of departure-target shortfall (keep it dominating).
    pub shortfall_usd_per_kwh: f64,
    /// Battery-wear cost per kWh discharged (building side).
    pub degradation_usd_per_kwh: f64,
}

impl Default for MpcConfig {
    fn default() -> Self {
        MpcConfig {
            lookahead_slots: 96,
            shortfall_usd_per_kwh: 1e6,
            degradation_usd_per_kwh: 0.05,
        }
    }
}

pub struct Mpc {
    backend: Box<dyn MilpBackend>,
    config: MpcConfig,
}

impl Mpc {
    pub fn new(backend: Box<dyn MilpBackend>, config: MpcConfig) -> Self {
        Mpc { backend, config }
    }
}

impl Policy for Mpc {
    fn name(&self) -> &'static str {
        "mpc"
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        if obs.sessions.is_empty() {
            return Vec::new();
        }
        let dt = obs.slot_minutes / 60.0;
        let now = obs.slot;
        let series_end = obs.building_series.len();
        let horizon_end = obs
            .sessions
            .iter()
            .map(|s| s.vehicle.departure_slot)
            .max()
            .expect("nonempty sessions")
            .min(series_end)
            .min(now + self.config.lookahead_slots);
        let n_slots = horizon_end.saturating_sub(now);
        if n_slots == 0 {
            return Vec::new();
        }

        let mut m = Model::default();

        // Per-session variables.
        struct SessVars {
            view_index: usize,
            first_slot: usize,
            last_slot: usize, // inclusive; < departure
            cp: Vec<VarId>,
            cn: Vec<VarId>,
        }
        let mut sessions: Vec<SessVars> = Vec::new();
        for view in &obs.sessions {
            let v = view.vehicle;
            let last_slot = (v.departure_slot - 1).min(horizon_end - 1);
            let tag = format!("v{}a{}", v.vehicle_id, v.arrival_slot);
            let mut cp = Vec::new();
            let mut cn = Vec::new();
            let mut e = Vec::new();
            for s in now..=last_slot {
                cp.push(m.add_var(format!("cp_{tag}_{s}"), 0.0, view.max_charge_kw * dt, 0.0));
                cn.push(m.add_var(
                    format!("cn_{tag}_{s}"),
                    0.0,
                    view.max_discharge_kw * dt,
                    self.config.degradation_usd_per_kwh,
                ));
                e.push(m.add_var(format!("e_{tag}_{s}"), v.min_soc_kwh, v.battery_kwh, 0.0));
            }
            // SoC recursion: e[s] - e[s-1] - eta_c*cp[s] + cn[s]/eta_d = 0,
            // anchored at the live SoC.
            for (k, s) in (now..=last_slot).enumerate() {
                let mut terms = vec![
                    (e[k], 1.0),
                    (cp[k], -obs.charge_efficiency),
                    (cn[k], 1.0 / obs.discharge_efficiency),
                ];
                let rhs = if k == 0 {
                    view.soc_kwh
                } else {
                    terms.push((e[k - 1], -1.0));
                    0.0
                };
                m.add_constraint(format!("soc_{tag}_{s}"), terms, Sense::Eq, rhs);
            }
            // Reachability terminal condition: after the modeled window the
            // vehicle can still add eta_c * max_charge * dt per remaining
            // slot; the terminal SoC plus a penalized shortfall must cover
            // the rest of the target.
            let slots_after = v.departure_slot - 1 - last_slot;
            let reachable = obs.charge_efficiency * view.max_charge_kw * dt * slots_after as f64;
            let needed = (v.soc_target_kwh - reachable).max(v.min_soc_kwh);
            let z = m.add_var(
                format!("z_{tag}"),
                0.0,
                f64::INFINITY,
                self.config.shortfall_usd_per_kwh,
            );
            m.add_constraint(
                format!("target_{tag}"),
                vec![(*e.last().expect("nonempty window"), 1.0), (z, 1.0)],
                Sense::Ge,
                needed,
            );
            sessions.push(SessVars {
                view_index: view.index,
                first_slot: now,
                last_slot,
                cp,
                cn,
            });
        }

        // Aggregate net load per modeled slot (kW), no-export and site cap.
        let cap = obs.site_cap_kw;
        let mut agg: Vec<VarId> = Vec::with_capacity(n_slots);
        for s in now..horizon_end {
            let building = obs.building_series[s];
            let ub = cap.map_or(f64::INFINITY, |c| c.max(building));
            let a = m.add_var(format!("agg_{s}"), 0.0, ub, 0.0);
            let mut terms = vec![(a, 1.0)];
            for sv in &sessions {
                if s >= sv.first_slot && s <= sv.last_slot {
                    let k = s - sv.first_slot;
                    terms.push((sv.cp[k], -1.0 / dt));
                    terms.push((sv.cn[k], 1.0 / dt));
                }
            }
            m.add_constraint(format!("aggdef_{s}"), terms, Sense::Eq, building);
            agg.push(a);
        }

        // Energy cost: price * (cp - cn) per slot (building energy constant).
        for sv in &sessions {
            for (k, s) in (sv.first_slot..=sv.last_slot).enumerate() {
                let price = obs.price_series[s];
                m.vars[sv.cp[k].0].obj += price;
                m.vars[sv.cn[k].0].obj -= price;
            }
        }

        // Demand components over the modeled window.
        if obs.demand_charge_usd_per_kw > 0.0 || obs.demand_charge_peak_usd_per_kw > 0.0 {
            let p_max = m.add_var("p_max", 0.0, f64::INFINITY, obs.demand_charge_usd_per_kw);
            let p_max_tou = m.add_var(
                "p_max_tou",
                0.0,
                f64::INFINITY,
                obs.demand_charge_peak_usd_per_kw,
            );
            for (k, s) in (now..horizon_end).enumerate() {
                m.add_constraint(
                    format!("peak_{s}"),
                    vec![(p_max, 1.0), (agg[k], -1.0)],
                    Sense::Ge,
                    0.0,
                );
                if obs.tou_series[s] == crate::scenario::TouClass::Peak {
                    m.add_constraint(
                        format!("peaktou_{s}"),
                        vec![(p_max_tou, 1.0), (agg[k], -1.0)],
                        Sense::Ge,
                        0.0,
                    );
                }
            }
        }

        // DR overflow: ov[s] >= (agg[s] - F) * dt on covered slots.
        for (ei, event) in obs.dr_events.iter().enumerate() {
            for (k, s) in (now..horizon_end).enumerate() {
                if event.contains(s) {
                    let ov = m.add_var(
                        format!("ov{ei}_{s}"),
                        0.0,
                        f64::INFINITY,
                        event.penalty_usd_per_kwh,
                    );
                    m.add_constraint(
                        format!("ovdef{ei}_{s}"),
                        vec![(ov, 1.0), (agg[k], -dt)],
                        Sense::Ge,
                        -dt * event.fsl_kw,
                    );
                }
            }
        }

        let solution = match self.backend.solve(&m) {
            Ok(s) if s.status == SolStatus::Optimal => s,
            _ => return Vec::new(), // engine-safe fallback: do nothing this slot
        };

        sessions
            .iter()
            .map(|sv| {
                let cp0 = solution.values[sv.cp[0].0];
                let cn0 = solution.values[sv.cn[0].0];
                Setpoint {
                    session_index: sv.view_index,
                    power_kw: (cp0 - cn0) / dt,
                }
            })
            .collect()
    }
}
