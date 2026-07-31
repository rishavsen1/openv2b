//! Scenario-MPC: the receding-horizon controller with K sampled futures,
//! matched to the reference's ILP-MPC semantics (see docs/OPTIMUS_PORT.md):
//!
//! - K scenarios = K historical episodes ("episodes" SAA source): each
//!   contributes its own FUTURE sessions (arrivals after the current slot,
//!   inside the horizon) AND its own building-load series, while the
//!   currently-connected sessions are shared. Sampling the building load is
//!   reference behavior: the controller does not assume it knows tomorrow's
//!   load, only that it looks like a historical day. Set
//!   `building_from_futures = false` to plan against the realized series
//!   instead (perfect building foresight; a diagnostic, not the reference).
//! - Horizon sawtooth: plan through the END OF THE NEXT DAY (1.0-2.0 days),
//!   never a fixed window.
//! - The objective is the UNNORMALIZED sum over scenarios (K x the mean),
//!   exactly like the reference; do not divide by K.
//! - Non-anticipativity: the first-slot rates of connected sessions are tied
//!   across scenarios; only scenario 0's first slot is committed.
//! - `p_max` carries a realized-history lower bound, ratcheted upward after
//!   each committed slot whose TOU class is peak (reference behavior).
//! - Ramp `q_delta` (default 1.25 kWh/slot) bounds consecutive-slot swings,
//!   matching the reference's always-on constraint.
//! - Shortfall slack at 1e6 $/kWh (the reference MPC's live soft target);
//!   battery wear at 0.05 $/kWh on discharge; no DR overflow term and no TOU
//!   discharge floor (both absent from the reference MPC formulation).
//!
//! Documented approximation (shared with the reference's own "episodes"
//! mode): future sessions come wholesale from each training episode; identity
//! collisions with currently-connected cars are not resolved, and in-plan
//! charger contention is not modeled.

use crate::milp::{MilpBackend, Model, Sense, SolStatus, VarId};
use crate::policy::Policy;
use crate::scenario::{Scenario, TouClass};
use crate::state::{Observation, Setpoint};
use std::cell::RefCell;

pub struct ScenarioMpcConfig {
    /// Sampled-future sources (converted historical episodes).
    pub futures: Vec<Scenario>,
    /// Ramp bound between consecutive slots, kWh/slot (reference: 1.25).
    pub ramp_kwh_per_slot: Option<f64>,
    pub shortfall_usd_per_kwh: f64,
    pub degradation_usd_per_kwh: f64,
    /// Slots per day for the horizon sawtooth (96 at 15-minute slots).
    pub slots_per_day: usize,
    /// Reference behavior: the peak variable carries a realized-history
    /// lower bound that ratchets upward (its `p_max_hist`). Disable to let
    /// every solve price the full peak (diagnostic / non-reference mode).
    pub use_peak_history: bool,
    /// Each scenario plans against ITS OWN episode's building-load series
    /// (the load is sampled, not known). False falls back to the past-only
    /// daily-persistence forecast. Neither setting reads the realized future
    /// series: no planner in this crate is given perfect building foresight.
    pub building_from_futures: bool,
    /// The test episode's session rows, used ONLY to source the
    /// between-visit consumption (`depletion_kwh`) of sampled future
    /// sessions belonging to tracked identities, matched by (identity,
    /// session index). This mirrors the reference, which merges its
    /// external-use column from the TEST episode.
    ///
    /// NOTE: this is a real information leak in the reference: the planner
    /// learns how far each car will actually be driven before its next
    /// visit. Leave empty to keep the honest behavior (use the sampled
    /// episode's own consumption).
    pub test_sessions: Vec<crate::scenario::Vehicle>,
}

impl ScenarioMpcConfig {
    pub fn new(futures: Vec<Scenario>) -> Self {
        ScenarioMpcConfig {
            futures,
            ramp_kwh_per_slot: Some(1.25),
            shortfall_usd_per_kwh: 1e6,
            degradation_usd_per_kwh: 0.05,
            slots_per_day: 96,
            use_peak_history: true,
            building_from_futures: true,
            test_sessions: Vec::new(),
        }
    }
}

pub struct ScenarioMpc {
    backend: Box<dyn MilpBackend>,
    config: ScenarioMpcConfig,
    /// Realized peak history (kW), the reference's `p_max_hist` ratchet.
    p_max_hist: RefCell<f64>,
}

impl ScenarioMpc {
    pub fn new(backend: Box<dyn MilpBackend>, config: ScenarioMpcConfig) -> Self {
        ScenarioMpc {
            backend,
            config,
            p_max_hist: RefCell::new(0.0),
        }
    }
}

/// One planned session (connected or sampled-future) inside one scenario.
struct Sess {
    /// Index into obs.sessions for connected sessions; None for futures.
    view_index: Option<usize>,
    first_slot: usize,
    last_slot: usize, // inclusive
    anchor_soc_kwh: f64,
    reach_needed_kwh: f64, // terminal requirement after horizon truncation
    floor_kwh: f64,
    ceiling_kwh: f64,
    max_charge_kw: f64,
    max_discharge_kw: f64,
    vehicle_id: u32,
    depletion_kwh: f64,
    cp: Vec<VarId>,
    cn: Vec<VarId>,
    last_e: Option<VarId>,
}

impl Policy for ScenarioMpc {
    fn name(&self) -> &'static str {
        "scenario-mpc"
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        if obs.sessions.is_empty() {
            return Vec::new();
        }
        let dt = obs.slot_minutes / 60.0;
        let now = obs.slot;
        let series_end = obs.building_series.len();
        // Sawtooth: through the end of the NEXT day.
        let day = now / self.config.slots_per_day;
        let horizon_end = ((day + 2) * self.config.slots_per_day).min(series_end);
        let n_scen = self.config.futures.len().max(1);

        let mut m = Model::default();
        let mut scen_sessions: Vec<Vec<Sess>> = Vec::with_capacity(n_scen);
        let deg = self.config.degradation_usd_per_kwh;

        // (identity, session index) -> the TEST episode's between-visit
        // consumption; see `test_sessions`.
        let mut test_depletion: std::collections::HashMap<(u32, usize), f64> =
            std::collections::HashMap::new();
        if !self.config.test_sessions.is_empty() {
            let mut rows: Vec<&crate::scenario::Vehicle> =
                self.config.test_sessions.iter().collect();
            rows.sort_by_key(|v| (v.vehicle_id, v.arrival_slot));
            let mut seq: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
            for v in rows {
                let idx = seq.entry(v.vehicle_id).or_insert(0);
                test_depletion.insert((v.vehicle_id, *idx), v.depletion_kwh);
                *idx += 1;
            }
        }

        for k in 0..n_scen {
            let mut sessions: Vec<Sess> = Vec::new();
            // Connected sessions: identical request data in every scenario.
            for view in &obs.sessions {
                let v = view.vehicle;
                let last = (v.departure_slot - 1).min(horizon_end - 1);
                sessions.push(Sess {
                    view_index: Some(view.index),
                    first_slot: now,
                    last_slot: last,
                    anchor_soc_kwh: view.soc_kwh,
                    reach_needed_kwh: reach_needed(
                        v.soc_target_kwh,
                        v.departure_slot,
                        last,
                        view.max_charge_kw,
                        dt,
                        obs.charge_efficiency,
                        v.min_soc_kwh,
                    ),
                    floor_kwh: v.min_soc_kwh,
                    ceiling_kwh: v.ceiling_kwh(),
                    max_charge_kw: view.max_charge_kw,
                    max_discharge_kw: view.max_discharge_kw,
                    vehicle_id: v.vehicle_id,
                    depletion_kwh: 0.0,
                    cp: vec![],
                    cn: vec![],
                    last_e: None,
                });
            }
            // Scenario k's sampled future arrivals.
            if let Some(future) = self.config.futures.get(k) {
                // Session index within the sampled episode, per identity
                // (the reference's composite id = car x 100 + session).
                let mut seq: std::collections::HashMap<u32, usize> =
                    std::collections::HashMap::new();
                let mut ordered: Vec<&crate::scenario::Vehicle> = future.vehicles.iter().collect();
                ordered.sort_by_key(|v| (v.vehicle_id, v.arrival_slot));
                for v in ordered {
                    let session_index = {
                        let e = seq.entry(v.vehicle_id).or_insert(0);
                        let i = *e;
                        *e += 1;
                        i
                    };
                    // Reference filter: arrival strictly after now AND
                    // departure strictly inside the horizon. Sessions that
                    // would be truncated by the horizon are DROPPED, not
                    // clipped (the reference's splice_state).
                    if v.arrival_slot > now && v.departure_slot < horizon_end {
                        let last = v.departure_slot - 1;
                        if last < v.arrival_slot {
                            continue;
                        }
                        // Deduplicate against the live state: a sampled
                        // session that overlaps a CURRENTLY CONNECTED session
                        // of the same identity is that car's present visit as
                        // the historical episode saw it, not a future one.
                        // The reference drops it (composite-id collision) and
                        // the live copy wins; keeping it would plan a phantom
                        // second copy of a car that is already plugged in.
                        if obs.sessions.iter().any(|view| {
                            view.vehicle.vehicle_id == v.vehicle_id
                                && v.arrival_slot < view.vehicle.departure_slot
                        }) {
                            continue;
                        }
                        sessions.push(Sess {
                            view_index: None,
                            first_slot: v.arrival_slot,
                            last_slot: last,
                            anchor_soc_kwh: v.soc_arrival_kwh,
                            reach_needed_kwh: reach_needed(
                                v.soc_target_kwh,
                                v.departure_slot,
                                last,
                                v.max_charge_kw,
                                dt,
                                obs.charge_efficiency,
                                v.min_soc_kwh,
                            ),
                            floor_kwh: v.min_soc_kwh,
                            ceiling_kwh: v.ceiling_kwh(),
                            max_charge_kw: v.max_charge_kw,
                            max_discharge_kw: v.max_discharge_kw,
                            vehicle_id: v.vehicle_id,
                            depletion_kwh: *test_depletion
                                .get(&(v.vehicle_id, session_index))
                                .unwrap_or(&v.depletion_kwh),
                            cp: vec![],
                            cn: vec![],
                            last_e: None,
                        });
                    }
                }
            }
            // Deterministic chain order: sessions of one identity in time
            // order, so a future session can link to its predecessor's
            // terminal energy (the reference's const-7 coupling: this is
            // what lets the plan bank in a connected session for the same
            // car's sampled future session).
            sessions.sort_by_key(|x| (x.vehicle_id, x.first_slot));
            // Variables + per-session constraints for this scenario.
            let mut prev_terminal: std::collections::HashMap<u32, VarId> =
                std::collections::HashMap::new();
            for (si, sess) in sessions.iter_mut().enumerate() {
                let tag = format!("k{k}s{si}");
                let mut e_vars = Vec::new();
                for s in sess.first_slot..=sess.last_slot {
                    sess.cp.push(m.add_var(
                        format!("cp_{tag}_{s}"),
                        0.0,
                        sess.max_charge_kw * dt,
                        obs.price_series[s],
                    ));
                    sess.cn.push(m.add_var(
                        format!("cn_{tag}_{s}"),
                        0.0,
                        sess.max_discharge_kw * dt,
                        deg - obs.price_series[s],
                    ));
                    e_vars.push(m.add_var(
                        format!("e_{tag}_{s}"),
                        sess.floor_kwh,
                        sess.ceiling_kwh,
                        0.0,
                    ));
                }
                let chained_prev = if sess.view_index.is_none() {
                    prev_terminal.get(&sess.vehicle_id).copied()
                } else {
                    None // connected sessions anchor at the LIVE SoC
                };
                for (i, s) in (sess.first_slot..=sess.last_slot).enumerate() {
                    let mut terms = vec![
                        (e_vars[i], 1.0),
                        (sess.cp[i], -obs.charge_efficiency),
                        (sess.cn[i], 1.0 / obs.discharge_efficiency),
                    ];
                    let rhs = if i > 0 {
                        terms.push((e_vars[i - 1], -1.0));
                        0.0
                    } else if let Some(prev_e) = chained_prev {
                        terms.push((prev_e, -1.0));
                        -sess.depletion_kwh
                    } else {
                        sess.anchor_soc_kwh
                    };
                    m.add_constraint(format!("soc_{tag}_{s}"), terms, Sense::Eq, rhs);
                }
                let z = m.add_var(
                    format!("z_{tag}"),
                    0.0,
                    f64::INFINITY,
                    self.config.shortfall_usd_per_kwh,
                );
                m.add_constraint(
                    format!("tgt_{tag}"),
                    vec![(*e_vars.last().expect("nonempty window"), 1.0), (z, 1.0)],
                    Sense::Ge,
                    sess.reach_needed_kwh,
                );
                if let Some(q) = self.config.ramp_kwh_per_slot {
                    for i in 1..sess.cp.len() {
                        let terms = vec![
                            (sess.cp[i], 1.0),
                            (sess.cn[i], -1.0),
                            (sess.cp[i - 1], -1.0),
                            (sess.cn[i - 1], 1.0),
                        ];
                        m.add_constraint(format!("rup_{tag}_{i}"), terms.clone(), Sense::Le, q);
                        m.add_constraint(format!("rdn_{tag}_{i}"), terms, Sense::Ge, -q);
                    }
                }
                sess.last_e = e_vars.last().copied();
                prev_terminal.insert(sess.vehicle_id, *e_vars.last().expect("nonempty window"));
            }
            scen_sessions.push(sessions);
        }

        // Per-scenario aggregate, no-export, and demand envelopes.
        let p_hist = if self.config.use_peak_history {
            *self.p_max_hist.borrow()
        } else {
            0.0
        };
        for (k, sessions) in scen_sessions.iter().enumerate() {
            let p_max_tou = m.add_var(
                format!("pmaxtou_k{k}"),
                p_hist,
                f64::INFINITY,
                obs.demand_charge_peak_usd_per_kw,
            );
            let p_max_all = m.add_var(
                format!("pmax_k{k}"),
                0.0,
                f64::INFINITY,
                obs.demand_charge_usd_per_kw,
            );
            for s in now..horizon_end {
                // Reference: scenario k prices its own episode's load; the
                // CURRENT slot uses the realized value (measured, not
                // forecast; sampling it too was tested and diverges further
                // from the reference's committed dispatch).
                let building = if self.config.building_from_futures && s > now {
                    // Sampled from this scenario's episode; if that episode
                    // is shorter, fall back to the PAST-ONLY forecast (never
                    // the realized future series).
                    self.config
                        .futures
                        .get(k)
                        .and_then(|f| f.building_load_kw.get(s).copied())
                        .unwrap_or_else(|| obs.building_forecast_kw(s))
                } else {
                    obs.building_forecast_kw(s)
                };
                let ub = obs.site_cap_kw.map_or(f64::INFINITY, |c| c.max(building));
                let a = m.add_var(format!("agg_k{k}_{s}"), 0.0, ub, 0.0);
                let mut terms = vec![(a, 1.0)];
                for sess in sessions {
                    if s >= sess.first_slot && s <= sess.last_slot {
                        let i = s - sess.first_slot;
                        terms.push((sess.cp[i], -1.0 / dt));
                        terms.push((sess.cn[i], 1.0 / dt));
                    }
                }
                m.add_constraint(format!("aggdef_k{k}_{s}"), terms, Sense::Eq, building);
                if obs.tou_series[s] == TouClass::Peak {
                    m.add_constraint(
                        format!("pk_k{k}_{s}"),
                        vec![(p_max_tou, 1.0), (a, -1.0)],
                        Sense::Ge,
                        0.0,
                    );
                }
                m.add_constraint(
                    format!("pka_k{k}_{s}"),
                    vec![(p_max_all, 1.0), (a, -1.0)],
                    Sense::Ge,
                    0.0,
                );
            }
        }

        // Non-anticipativity: connected sessions' first-slot rates are tied
        // to scenario 0, paired by their observation view index (session
        // order inside a scenario interleaves sampled futures, so positional
        // pairing would be wrong).
        let base_by_view: std::collections::HashMap<usize, (VarId, VarId)> = scen_sessions[0]
            .iter()
            .filter_map(|sess| sess.view_index.map(|vi| (vi, (sess.cp[0], sess.cn[0]))))
            .collect();
        for (k, sessions) in scen_sessions.iter().enumerate().skip(1) {
            for (si, sess) in sessions.iter().enumerate() {
                if let Some(vi) = sess.view_index {
                    let (base_cp, base_cn) = base_by_view[&vi];
                    m.add_constraint(
                        format!("na_cp_k{k}s{si}"),
                        vec![(sess.cp[0], 1.0), (base_cp, -1.0)],
                        Sense::Eq,
                        0.0,
                    );
                    m.add_constraint(
                        format!("na_cn_k{k}s{si}"),
                        vec![(sess.cn[0], 1.0), (base_cn, -1.0)],
                        Sense::Eq,
                        0.0,
                    );
                }
            }
        }

        let solution = match self.backend.solve(&m) {
            Ok(s) if s.status == SolStatus::Optimal => s,
            _ => return Vec::new(), // engine-safe fallback
        };

        // Commit scenario 0's first slot; ratchet the peak history when the
        // committed slot is peak-TOU class (reference update rule).
        let setpoints: Vec<Setpoint> = scen_sessions[0]
            .iter()
            .filter_map(|sess| {
                sess.view_index.map(|idx| Setpoint {
                    session_index: idx,
                    power_kw: (solution.values[sess.cp[0].0] - solution.values[sess.cn[0].0]) / dt,
                })
            })
            .collect();
        if self.config.use_peak_history && obs.tou == TouClass::Peak {
            let committed: f64 = setpoints.iter().map(|sp| sp.power_kw).sum();
            let realized = obs.building_load_kw + committed;
            let mut hist = self.p_max_hist.borrow_mut();
            if realized > *hist {
                *hist = realized;
            }
        }
        setpoints
    }
}

/// Terminal requirement after horizon truncation: the target minus what the
/// session can still add at full power after the modeled window, floored at
/// the SoC floor (same reachability shape as the deterministic MPC).
#[allow(clippy::too_many_arguments)]
fn reach_needed(
    target_kwh: f64,
    departure_slot: usize,
    last_modeled_slot: usize,
    max_charge_kw: f64,
    dt: f64,
    eta_c: f64,
    floor_kwh: f64,
) -> f64 {
    let slots_after = departure_slot - 1 - last_modeled_slot;
    let reachable = eta_c * max_charge_kw * dt * slots_after as f64;
    (target_kwh - reachable).max(floor_kwh)
}
