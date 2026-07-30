//! Full-horizon oracle: solve the whole scenario once with perfect foresight
//! (all sessions known, persistence chains coupled), then replay the schedule
//! open-loop. The oracle is the reference point for receding-horizon (MPC)
//! parity testing, and the vehicle for firm-service-level (FSL) commitment
//! optimization.
//!
//! Scope restrictions (checked, not assumed): every charger port must have
//! the same power limit, and there must be at least as many ports as
//! concurrently-connected sessions (no queueing), because the LP does not
//! model charger contention. Fixtures satisfying this keep the oracle exact.
//!
//! IMPORTANT (inherited lesson): the oracle's bill is not a lower bound over
//! bills. The objective contains an unbilled battery-wear term, so a policy
//! may realize a bill up to that term below the oracle's. Compare bills only
//! with that slack in mind, or compare objectives.

use crate::milp::{MilpBackend, Model, Sense, SolStatus, SolveError, VarId};
use crate::policy::Policy;
use crate::scenario::Scenario;
use crate::state::{Observation, Setpoint};
use std::collections::HashMap;

pub struct OracleConfig {
    /// Penalty per kWh of departure-target shortfall.
    pub shortfall_usd_per_kwh: f64,
    /// Battery-wear cost per kWh discharged (building side). Unbilled.
    pub degradation_usd_per_kwh: f64,
    /// Optimize each DR event's firm service level as a decision variable in
    /// [0, counterfactual baseline peak] instead of taking `fsl_kw` as given.
    pub optimize_fsl: bool,
}

impl Default for OracleConfig {
    fn default() -> Self {
        OracleConfig {
            shortfall_usd_per_kwh: 1e6,
            degradation_usd_per_kwh: 0.05,
            optimize_fsl: false,
        }
    }
}

/// The oracle's solved plan.
pub struct OraclePlan {
    /// Setpoint per (vehicle_id, arrival_slot, slot), kW (signed like Setpoint).
    schedule: HashMap<(u32, usize, usize), f64>,
    /// Planned objective value (includes unbilled terms).
    pub objective: f64,
    /// Per DR event (input order): the committed firm level. Equal to the
    /// input `fsl_kw` unless `optimize_fsl` was set.
    pub committed_fsl_kw: Vec<f64>,
    /// Per DR event: the counterfactual no-DR baseline in-window peak, kW
    /// (only computed when `optimize_fsl` is set; otherwise the input value).
    pub baseline_peak_kw: Vec<f64>,
}

impl OraclePlan {
    pub fn power_at(&self, vehicle_id: u32, arrival_slot: usize, slot: usize) -> f64 {
        *self
            .schedule
            .get(&(vehicle_id, arrival_slot, slot))
            .unwrap_or(&0.0)
    }
}

/// Replay policy: executes a precomputed oracle plan through the normal
/// engine (which still clamps everything).
pub struct OracleReplay {
    pub plan: OraclePlan,
}

impl Policy for OracleReplay {
    fn name(&self) -> &'static str {
        "oracle"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: self.plan.power_at(
                    s.vehicle.vehicle_id,
                    s.vehicle.arrival_slot,
                    obs.slot,
                ),
            })
            .collect()
    }
}

/// Solve the full-horizon problem. See module docs for scope restrictions.
pub fn solve_oracle(
    scenario: &Scenario,
    backend: &dyn MilpBackend,
    config: &OracleConfig,
) -> Result<OraclePlan, SolveError> {
    check_scope(scenario)?;

    // Pass 1 (only for FSL optimization): the counterfactual baseline solve
    // with an empty DR slot set. Its in-window peak bounds the commitment.
    let baseline_peak_kw: Vec<f64> = if config.optimize_fsl {
        let base = build_and_solve(scenario, backend, config, false, None)?;
        scenario
            .dr_events
            .iter()
            .map(|e| {
                (0..scenario.manifest.horizon_slots)
                    .filter(|&s| e.contains(s))
                    .map(|s| base.net_kw[s])
                    .fold(0.0f64, f64::max)
            })
            .collect()
    } else {
        scenario.dr_events.iter().map(|e| e.baseline_kw).collect()
    };

    let solved = build_and_solve(
        scenario,
        backend,
        config,
        true,
        if config.optimize_fsl {
            Some(&baseline_peak_kw)
        } else {
            None
        },
    )?;

    // Honored-gate post-adjustment (FSL optimization only). The LP prices the
    // incentive linearly, but the billing layer pays it ALL-OR-NOTHING on a
    // honored window: on short windows the LP may commit below the coverable
    // level (a few slots of penalty look cheaper than the one-shot incentive
    // forgone). Given the solved dispatch, evaluate each event's gated cost
    // at the LP's F versus at the realized in-window peak (which honors by
    // construction) and keep the cheaper commitment.
    let dt = scenario.manifest.slot_minutes / 60.0;
    let mut committed = solved.committed_fsl_kw.clone();
    if config.optimize_fsl {
        for (ei, event) in scenario.dr_events.iter().enumerate() {
            let in_window: Vec<f64> = (0..scenario.manifest.horizon_slots)
                .filter(|&s| event.contains(s))
                .map(|s| solved.net_kw[s])
                .collect();
            let realized_peak = in_window.iter().copied().fold(0.0f64, f64::max);
            let gated_cost = |f: f64| -> f64 {
                let overflow: f64 = in_window.iter().map(|&n| (n - f).max(0.0) * dt).sum();
                let honored = overflow <= 1e-9;
                event.penalty_usd_per_kwh * overflow
                    - if honored {
                        event.incentive_usd_per_kw * (baseline_peak_kw[ei] - f).max(0.0)
                    } else {
                        0.0
                    }
            };
            if gated_cost(realized_peak) < gated_cost(committed[ei]) {
                committed[ei] = realized_peak;
            }
        }
    }

    Ok(OraclePlan {
        schedule: solved.schedule,
        objective: solved.objective,
        committed_fsl_kw: committed,
        baseline_peak_kw,
    })
}

fn check_scope(scenario: &Scenario) -> Result<(), SolveError> {
    let first = scenario
        .chargers
        .first()
        .ok_or_else(|| SolveError::Backend("oracle: no chargers".into()))?;
    if scenario.chargers.iter().any(|c| c.max_kw != first.max_kw) {
        return Err(SolveError::Backend(
            "oracle: heterogeneous charger power limits are out of scope".into(),
        ));
    }
    let needs_bidi = scenario.vehicles.iter().any(|v| v.max_discharge_kw > 0.0);
    let bidi_ports = scenario.chargers.iter().filter(|c| c.bidirectional).count();
    for slot in 0..scenario.manifest.horizon_slots {
        let connected = scenario
            .vehicles
            .iter()
            .filter(|v| v.arrival_slot <= slot && slot < v.departure_slot)
            .count();
        if connected > scenario.chargers.len() {
            return Err(SolveError::Backend(format!(
                "oracle: charger contention at slot {slot} is out of scope"
            )));
        }
        if needs_bidi {
            let v2b_connected = scenario
                .vehicles
                .iter()
                .filter(|v| {
                    v.max_discharge_kw > 0.0 && v.arrival_slot <= slot && slot < v.departure_slot
                })
                .count();
            if v2b_connected > bidi_ports {
                return Err(SolveError::Backend(format!(
                    "oracle: more V2B sessions than bidirectional ports at slot {slot}"
                )));
            }
        }
    }
    Ok(())
}

struct SolvedModel {
    schedule: HashMap<(u32, usize, usize), f64>,
    objective: f64,
    net_kw: Vec<f64>,
    committed_fsl_kw: Vec<f64>,
}

/// Build and solve the full-horizon LP. `with_dr` gates the DR overflow and
/// FSL terms; `fsl_bounds` (per event upper bounds) switches the firm level
/// from a constant to a decision variable.
fn build_and_solve(
    scenario: &Scenario,
    backend: &dyn MilpBackend,
    config: &OracleConfig,
    with_dr: bool,
    fsl_bounds: Option<&[f64]>,
) -> Result<SolvedModel, SolveError> {
    let m = &scenario.manifest;
    let dt = m.slot_minutes / 60.0;
    let eta_c = m.charge_efficiency;
    let eta_d = m.discharge_efficiency;
    let horizon = m.horizon_slots;
    let port_kw = scenario.chargers[0].max_kw;

    let mut lp = Model::default();

    // Session variables, chained per vehicle in arrival order.
    struct Sess {
        vehicle_index: usize,
        cp: Vec<VarId>,
        cn: Vec<VarId>,
    }
    let mut by_vehicle: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut order: Vec<usize> = (0..scenario.vehicles.len()).collect();
    order.sort_by_key(|&i| {
        (
            scenario.vehicles[i].arrival_slot,
            scenario.vehicles[i].vehicle_id,
        )
    });
    for &i in &order {
        by_vehicle
            .entry(scenario.vehicles[i].vehicle_id)
            .or_default()
            .push(i);
    }

    let mut sessions: Vec<Sess> = Vec::new();
    let mut terminal_e_of_row: HashMap<usize, VarId> = HashMap::new();
    for &i in &order {
        let v = &scenario.vehicles[i];
        let tag = format!("v{}a{}", v.vehicle_id, v.arrival_slot);
        let max_chg = v.max_charge_kw.min(port_kw);
        let max_dis = v.max_discharge_kw.min(port_kw);
        let mut cp = Vec::new();
        let mut cn = Vec::new();
        let mut e = Vec::new();
        for s in v.arrival_slot..v.departure_slot {
            cp.push(lp.add_var(format!("cp_{tag}_{s}"), 0.0, max_chg * dt, 0.0));
            cn.push(lp.add_var(
                format!("cn_{tag}_{s}"),
                0.0,
                max_dis * dt,
                config.degradation_usd_per_kwh,
            ));
            e.push(lp.add_var(format!("e_{tag}_{s}"), v.min_soc_kwh, v.battery_kwh, 0.0));
        }
        // SoC recursion. First slot anchors to the CSV arrival SoC or, for a
        // chained session under persistence, to the previous session's
        // terminal energy variable minus the depletion (LP-coupled: this is
        // exactly what lets the oracle bank across days).
        let chain_prev: Option<usize> = if m.persistence {
            let chain = &by_vehicle[&v.vehicle_id];
            let pos = chain.iter().position(|&r| r == i).expect("row in chain");
            (pos > 0).then(|| chain[pos - 1])
        } else {
            None
        };
        for (k, s) in (v.arrival_slot..v.departure_slot).enumerate() {
            let mut terms = vec![(e[k], 1.0), (cp[k], -eta_c), (cn[k], 1.0 / eta_d)];
            let rhs = if k > 0 {
                terms.push((e[k - 1], -1.0));
                0.0
            } else if let Some(prev_row) = chain_prev {
                terms.push((terminal_e_of_row[&prev_row], -1.0));
                -v.depletion_kwh
            } else {
                v.soc_arrival_kwh
            };
            lp.add_constraint(format!("soc_{tag}_{s}"), terms, Sense::Eq, rhs);
        }
        // Departure requirement with penalized shortfall.
        let z = lp.add_var(
            format!("z_{tag}"),
            0.0,
            f64::INFINITY,
            config.shortfall_usd_per_kwh,
        );
        let last = *e.last().expect("session has at least one slot");
        lp.add_constraint(
            format!("target_{tag}"),
            vec![(last, 1.0), (z, 1.0)],
            Sense::Ge,
            v.soc_target_kwh,
        );
        terminal_e_of_row.insert(i, last);
        sessions.push(Sess {
            vehicle_index: i,
            cp,
            cn,
        });
    }

    // Aggregate net load per slot, energy cost, no-export, site cap.
    let mut agg: Vec<VarId> = Vec::with_capacity(horizon);
    for s in 0..horizon {
        let building = scenario.building_load_kw[s];
        let ub = m.site_cap_kw.map_or(f64::INFINITY, |c| c.max(building));
        let a = lp.add_var(format!("agg_{s}"), 0.0, ub, 0.0);
        let mut terms = vec![(a, 1.0)];
        for sess in &sessions {
            let v = &scenario.vehicles[sess.vehicle_index];
            if s >= v.arrival_slot && s < v.departure_slot {
                let k = s - v.arrival_slot;
                terms.push((sess.cp[k], -1.0 / dt));
                terms.push((sess.cn[k], 1.0 / dt));
            }
        }
        lp.add_constraint(format!("aggdef_{s}"), terms, Sense::Eq, building);
        agg.push(a);
    }
    for sess in &sessions {
        let v = &scenario.vehicles[sess.vehicle_index];
        for (k, s) in (v.arrival_slot..v.departure_slot).enumerate() {
            let price = scenario.price_usd_per_kwh[s];
            lp.vars[sess.cp[k].0].obj += price;
            lp.vars[sess.cn[k].0].obj -= price;
        }
    }

    // Demand components.
    if m.demand_charge_usd_per_kw > 0.0 || m.demand_charge_peak_usd_per_kw > 0.0 {
        let p_max = lp.add_var("p_max", 0.0, f64::INFINITY, m.demand_charge_usd_per_kw);
        let p_max_tou = lp.add_var(
            "p_max_tou",
            0.0,
            f64::INFINITY,
            m.demand_charge_peak_usd_per_kw,
        );
        for (s, &a) in agg.iter().enumerate() {
            lp.add_constraint(
                format!("peak_{s}"),
                vec![(p_max, 1.0), (a, -1.0)],
                Sense::Ge,
                0.0,
            );
            if scenario.tou_class[s] == crate::scenario::TouClass::Peak {
                lp.add_constraint(
                    format!("peaktou_{s}"),
                    vec![(p_max_tou, 1.0), (a, -1.0)],
                    Sense::Ge,
                    0.0,
                );
            }
        }
    }

    // DR terms. With a fixed firm level: ov >= (agg - F)*dt. With FSL
    // optimization: F is a variable in [0, baseline]; the incentive
    // rate*(baseline - F) contributes +rate*F to the minimization (constant
    // dropped) so the planner is paid to commit deeper, balanced against the
    // overflow penalty. The all-or-nothing honored gate of the billing layer
    // is NOT modeled here (a soft-penalty approximation); `solve_oracle`
    // callers get honored plans in practice because the shortfall penalty on
    // overflow dominates at optimality when the commitment is chosen by the
    // same solve.
    let mut fsl_vars: Vec<Option<VarId>> = vec![None; scenario.dr_events.len()];
    if with_dr {
        for (ei, event) in scenario.dr_events.iter().enumerate() {
            let fsl_var = fsl_bounds.map(|bounds| {
                lp.add_var(
                    format!("fsl_{ei}"),
                    0.0,
                    bounds[ei],
                    event.incentive_usd_per_kw,
                )
            });
            fsl_vars[ei] = fsl_var;
            for (s, &a) in agg.iter().enumerate() {
                if event.contains(s) {
                    let ov = lp.add_var(
                        format!("ov{ei}_{s}"),
                        0.0,
                        f64::INFINITY,
                        event.penalty_usd_per_kwh,
                    );
                    let mut terms = vec![(ov, 1.0), (a, -dt)];
                    let rhs = match fsl_var {
                        Some(f) => {
                            terms.push((f, dt));
                            0.0
                        }
                        None => -dt * event.fsl_kw,
                    };
                    lp.add_constraint(format!("ovdef{ei}_{s}"), terms, Sense::Ge, rhs);
                }
            }
        }
    }

    let solution = backend.solve(&lp)?;
    if solution.status != SolStatus::Optimal {
        return Err(SolveError::Backend("oracle LP not optimal".into()));
    }

    let mut schedule = HashMap::new();
    for sess in &sessions {
        let v = &scenario.vehicles[sess.vehicle_index];
        for (k, s) in (v.arrival_slot..v.departure_slot).enumerate() {
            let kw = (solution.values[sess.cp[k].0] - solution.values[sess.cn[k].0]) / dt;
            schedule.insert((v.vehicle_id, v.arrival_slot, s), kw);
        }
    }
    let net_kw: Vec<f64> = (0..horizon).map(|s| solution.values[agg[s].0]).collect();
    let committed_fsl_kw: Vec<f64> = scenario
        .dr_events
        .iter()
        .enumerate()
        .map(|(ei, e)| match fsl_vars[ei] {
            Some(f) => solution.values[f.0],
            None => e.fsl_kw,
        })
        .collect();

    Ok(SolvedModel {
        schedule,
        objective: solution.objective,
        net_kw,
        committed_fsl_kw,
    })
}
