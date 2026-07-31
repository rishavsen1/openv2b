//! The simulation loop.
//!
//! Time advances in fixed slots. At each slot boundary the engine processes
//! departures, then arrivals (deterministic order), asks the policy for power
//! setpoints, clamps them against physical limits, and integrates energy for
//! the slot. The engine, not the policy, is the authority on feasibility: a
//! policy that requests infeasible power gets silently clamped, so every
//! physical invariant holds for every policy.

use crate::billing::{compute_bill, Bill};
use crate::policy::Policy;
use crate::scenario::{Scenario, TouClass, Vehicle};
use crate::state::{Observation, Session, SessionView, Setpoint};
use crate::{kw_to_kwh, kwh_to_kw};
use std::collections::HashMap;

/// Per-slot record of the site's power flows, kW.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SlotRecord {
    pub slot: usize,
    pub building_kw: f64,
    pub ev_charge_kw: f64,
    pub ev_discharge_kw: f64,
    /// building + charge - discharge; never negative (no-export guard).
    pub net_kw: f64,
    pub price_usd_per_kwh: f64,
    pub tou: TouClass,
}

/// One session's power and SoC in one slot (written to `trace.csv`); lets an
/// external checker verify charger exclusivity, queue fairness, and exact
/// per-session trajectories.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TraceRecord {
    pub slot: usize,
    pub vehicle_id: u32,
    pub arrival_slot: usize,
    pub charger_id: u32,
    /// Applied power this slot: positive grid-side charge, negative
    /// building-side discharge, kW.
    pub power_kw: f64,
    /// SoC at the END of the slot, kWh.
    pub soc_kwh: f64,
}

/// Outcome of one vehicle session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResult {
    pub vehicle_id: u32,
    pub arrival_slot: usize,
    pub departure_slot: usize,
    /// Actual SoC at arrival: the CSV value, or the persistence-chained value
    /// (previous departure SoC minus depletion, clamped) for later sessions.
    pub soc_arrival_kwh: f64,
    pub soc_departure_kwh: f64,
    pub soc_target_kwh: f64,
    pub target_met: bool,
    /// Missing charge at departure: max(0, target - departure SoC), kWh.
    pub missing_kwh: f64,
    /// Banked (excess) charge at departure: max(0, departure SoC - target), kWh.
    pub banked_kwh: f64,
    /// Grid energy drawn for this session, kWh (meter side).
    pub energy_drawn_kwh: f64,
    /// Energy exported to the building, kWh (building side).
    pub energy_exported_kwh: f64,
    /// Energy manufactured by the persistence clamp at this session's arrival:
    /// nonzero when the declared depletion exceeded what the battery held
    /// (the trip was physically infeasible as declared). Always reported,
    /// never silent.
    pub chain_clamped_kwh: f64,
    /// True if the session never obtained a charger before departing.
    pub never_connected: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Results {
    pub policy: String,
    pub slots: Vec<SlotRecord>,
    pub sessions: Vec<SessionResult>,
    pub trace: Vec<TraceRecord>,
    pub bill: Bill,
}

/// Run `policy` over `scenario` and return the full result set.
pub fn run(scenario: &Scenario, policy: &dyn Policy) -> Results {
    let m = &scenario.manifest;
    let dt_min = m.slot_minutes;
    let eta_c = m.charge_efficiency;
    let eta_d = m.discharge_efficiency;

    // Sessions indexed like scenario.vehicles; None until arrival or after departure.
    let mut active: Vec<Option<Session>> = vec![None; scenario.vehicles.len()];
    let mut charger_free = vec![true; scenario.chargers.len()];
    let mut results_sessions: Vec<SessionResult> = Vec::with_capacity(scenario.vehicles.len());
    let mut slots: Vec<SlotRecord> = Vec::with_capacity(m.horizon_slots);

    // Effective arrival SoC per session row; overwritten at arrival time when
    // persistence chains it to the previous session of the same vehicle.
    let mut arrival_soc: Vec<f64> = scenario
        .vehicles
        .iter()
        .map(|v| v.soc_arrival_kwh)
        .collect();
    // Energy manufactured by the arrival clamp per session row (see
    // `SessionResult::chain_clamped_kwh`).
    let mut chain_clamped: Vec<f64> = vec![0.0; scenario.vehicles.len()];
    // Last observed SoC per vehicle identity (updated at each departure).
    let mut chain_soc: HashMap<u32, f64> = HashMap::new();
    let mut trace: Vec<TraceRecord> = Vec::new();

    // Arrival processing order: (arrival_slot, vehicle_id) for determinism.
    let mut arrival_order: Vec<usize> = (0..scenario.vehicles.len()).collect();
    arrival_order.sort_by_key(|&i| {
        (
            scenario.vehicles[i].arrival_slot,
            scenario.vehicles[i].vehicle_id,
        )
    });

    // This slot's arrivals awaiting assignment (emptied every slot: the
    // reference drops unassignable cars instead of queueing them).
    let mut waiting: Vec<usize> = Vec::new();
    // Sessions that arrived but never obtained a charger.
    let mut dropped: Vec<bool> = vec![false; scenario.vehicles.len()];

    for slot in 0..m.horizon_slots {
        // 1. Departures: a vehicle whose departure_slot == slot is gone before
        //    this slot's decision. Chain state updates here, so a same-slot
        //    handoff to the vehicle's next session sees the fresh SoC.
        for (i, slot_state) in active.iter_mut().enumerate() {
            let v = &scenario.vehicles[i];
            if v.departure_slot == slot {
                if let Some(session) = slot_state.take() {
                    charger_free[session.charger_index] = true;
                    chain_soc.insert(v.vehicle_id, session.soc_kwh);
                    results_sessions.push(finish(v, &session, chain_clamped[i], false));
                } else if dropped[i] {
                    // Arrived but never obtained a charger: report the
                    // unserved session instead of losing it silently (and
                    // only once: the horizon-end sweep must not re-report).
                    dropped[i] = false;
                    let session = unserved_session(i, arrival_soc[i]);
                    chain_soc.insert(v.vehicle_id, session.soc_kwh);
                    results_sessions.push(finish(v, &session, chain_clamped[i], true));
                }
            }
        }

        // 2. Arrivals: resolve the effective arrival SoC (persistence) and
        //    join the waiting queue.
        for &i in &arrival_order {
            let v = &scenario.vehicles[i];
            if v.arrival_slot == slot {
                if m.persistence {
                    if let Some(&prev_soc) = chain_soc.get(&v.vehicle_id) {
                        let raw = prev_soc - v.depletion_kwh;
                        let clamped = raw.clamp(v.min_soc_kwh, v.ceiling_kwh());
                        arrival_soc[i] = clamped;
                        // Energy the clamp invented (declared trip infeasible).
                        chain_clamped[i] = (clamped - raw).max(0.0);
                    }
                }
                waiting.push(i);
            }
        }

        // 3. Assignment, reference semantics: this slot's waiting cars are
        //    processed in ascending vehicle-id order; each takes the first
        //    vacant port with bidirectional ports preferred (ties: lowest
        //    charger id, our stable-tie divergence); a car that finds no
        //    vacant port is DROPPED permanently (never retried), exactly as
        //    the reference does, and is reported `never_connected` at its
        //    departure.
        waiting.sort_by_key(|&i| scenario.vehicles[i].vehicle_id);
        for &i in waiting.iter() {
            let pick = (0..scenario.chargers.len())
                .filter(|&c| charger_free[c])
                .min_by_key(|&c| (!scenario.chargers[c].bidirectional, c));
            match pick {
                Some(c) => {
                    charger_free[c] = false;
                    active[i] = Some(Session {
                        vehicle_index: i,
                        charger_index: c,
                        soc_arrival_kwh: arrival_soc[i],
                        soc_kwh: arrival_soc[i],
                        energy_drawn_kwh: 0.0,
                        energy_exported_kwh: 0.0,
                    });
                }
                None => dropped[i] = true,
            }
        }
        waiting.clear();

        // 4. Build the observation and ask the policy.
        let dr_fsl_kw = scenario
            .dr_events
            .iter()
            .filter(|e| e.contains(slot))
            .map(|e| e.fsl_kw)
            .fold(None, |acc: Option<f64>, f| {
                Some(acc.map_or(f, |a| a.min(f)))
            });

        // Canonical session order: (arrival_slot, vehicle_id), NOT the CSV
        // row order. This makes the observation - and therefore every
        // policy's emission order - invariant to input-row permutation
        // (audit F2: with a binding site cap, row-ordered rationing changed
        // outcomes when CSV rows were shuffled).
        let mut view_to_vehicle: Vec<usize> =
            active.iter().flatten().map(|s| s.vehicle_index).collect();
        view_to_vehicle.sort_by_key(|&i| {
            (
                scenario.vehicles[i].arrival_slot,
                scenario.vehicles[i].vehicle_id,
            )
        });
        let session_views: Vec<SessionView> = view_to_vehicle
            .iter()
            .enumerate()
            .map(|(view_index, &vehicle_index)| {
                let s = active[vehicle_index]
                    .as_ref()
                    .expect("view_to_vehicle only maps active sessions");
                let v = &scenario.vehicles[vehicle_index];
                let charger = &scenario.chargers[s.charger_index];
                SessionView {
                    index: view_index,
                    vehicle: v,
                    soc_kwh: s.soc_kwh,
                    max_charge_kw: v.max_charge_kw.min(charger.max_kw),
                    max_discharge_kw: if charger.bidirectional {
                        v.max_discharge_kw.min(charger.max_kw)
                    } else {
                        0.0
                    },
                }
            })
            .collect();
        let n_views = view_to_vehicle.len();

        let obs = Observation {
            slot,
            slot_minutes: dt_min,
            building_load_kw: scenario.building_load_kw[slot],
            price_usd_per_kwh: scenario.price_usd_per_kwh[slot],
            tou: scenario.tou_class[slot],
            site_cap_kw: m.site_cap_kw,
            charge_efficiency: eta_c,
            discharge_efficiency: eta_d,
            dr_fsl_kw,
            sessions: session_views,
            price_series: &scenario.price_usd_per_kwh,
            building_series: &scenario.building_load_kw,
            tou_series: &scenario.tou_class,
            dr_events: &scenario.dr_events,
            heuristic_threshold_kw: m.heuristic_threshold_kw,
            demand_charge_usd_per_kw: m.demand_charge_usd_per_kw,
            demand_charge_peak_usd_per_kw: m.demand_charge_peak_usd_per_kw,
        };

        // One setpoint per session at most: a later setpoint for the same
        // session overrides an earlier one, so no session can charge and
        // discharge in the same slot. Out-of-range indices and non-finite
        // powers (NaN, +/-inf would otherwise slip past both sign guards)
        // are discarded before dedup: the engine, not the policy, owns
        // feasibility. The POLICY'S EMISSION ORDER is preserved (audit F1):
        // scarce headroom (site cap, no-export) is rationed in the order the
        // policy asked, with an overridden setpoint keeping its latest
        // emission position.
        let mut requested: Vec<Setpoint> = Vec::new();
        for sp in policy.decide(&obs) {
            if sp.session_index < n_views && sp.power_kw.is_finite() {
                requested.retain(|old| old.session_index != sp.session_index);
                requested.push(sp);
            }
        }

        // 5. Clamp and integrate. The engine re-derives limits so no policy
        //    can violate physics. Charging is applied first; discharging is
        //    applied second under a no-export guard: aggregate discharge may
        //    offset the site's draw (building + charging) but never exceed it,
        //    so net load stays non-negative.
        let building_kw = scenario.building_load_kw[slot];
        let mut applied_kw_by_view = vec![0.0; n_views];

        // Charging first, under the site cap: total EV charging may not push
        // the site above `site_cap_kw` (the cap binds the EV fleet; the
        // building's own load is not curtailable).
        let mut charge_headroom_kw = m
            .site_cap_kw
            .map_or(f64::INFINITY, |cap| (cap - building_kw).max(0.0));
        let mut charge_kw_total = 0.0;
        for sp in &requested {
            if sp.power_kw < 0.0 {
                continue;
            }
            let view = &obs.sessions[sp.session_index];
            let session = active[view_to_vehicle[sp.session_index]]
                .as_mut()
                .expect("view_to_vehicle only maps active sessions");
            let applied =
                apply_setpoint(session, view, sp, dt_min, eta_c, eta_d, charge_headroom_kw);
            applied_kw_by_view[sp.session_index] = applied;
            charge_kw_total += applied;
            charge_headroom_kw -= applied;
        }
        // Discharging second, under the no-export guard.
        let mut export_headroom_kw = building_kw + charge_kw_total;
        let mut discharge_kw_total = 0.0;
        for sp in &requested {
            if sp.power_kw >= 0.0 {
                continue;
            }
            let view = &obs.sessions[sp.session_index];
            let session = active[view_to_vehicle[sp.session_index]]
                .as_mut()
                .expect("view_to_vehicle only maps active sessions");
            let applied_kw =
                apply_setpoint(session, view, sp, dt_min, eta_c, eta_d, export_headroom_kw);
            applied_kw_by_view[sp.session_index] = applied_kw;
            discharge_kw_total -= applied_kw;
            export_headroom_kw += applied_kw;
        }

        for (view_index, &vehicle_index) in view_to_vehicle.iter().enumerate() {
            let session = active[vehicle_index]
                .as_ref()
                .expect("view_to_vehicle only maps active sessions");
            let v = &scenario.vehicles[vehicle_index];
            trace.push(TraceRecord {
                slot,
                vehicle_id: v.vehicle_id,
                arrival_slot: v.arrival_slot,
                charger_id: scenario.chargers[session.charger_index].charger_id,
                power_kw: applied_kw_by_view[view_index],
                soc_kwh: session.soc_kwh,
            });
        }

        slots.push(SlotRecord {
            slot,
            building_kw,
            ev_charge_kw: charge_kw_total,
            ev_discharge_kw: discharge_kw_total,
            net_kw: building_kw + charge_kw_total - discharge_kw_total,
            price_usd_per_kwh: scenario.price_usd_per_kwh[slot],
            tou: scenario.tou_class[slot],
        });
    }

    // Sessions still plugged in at horizon end (or never connected).
    for (i, slot_state) in active.iter_mut().enumerate() {
        let v = &scenario.vehicles[i];
        if let Some(session) = slot_state.take() {
            results_sessions.push(finish(v, &session, chain_clamped[i], false));
        } else if dropped[i] {
            results_sessions.push(finish(
                v,
                &unserved_session(i, arrival_soc[i]),
                chain_clamped[i],
                true,
            ));
        }
    }
    results_sessions.sort_by_key(|r| (r.arrival_slot, r.vehicle_id));

    let bill = compute_bill(scenario, &slots);
    Results {
        policy: policy.name().to_string(),
        slots,
        sessions: results_sessions,
        trace,
        bill,
    }
}

/// Clamp a requested setpoint to physical limits, update the session's battery
/// state, and return the power actually applied (grid/building side, kW).
/// `site_cap_kw` is the remaining site-level allowance for this request: the
/// charge headroom under the site cap for charging requests, or the remaining
/// offsettable draw (no-export guard) for discharge requests.
fn apply_setpoint(
    session: &mut Session,
    view: &SessionView,
    sp: &Setpoint,
    dt_min: f64,
    eta_c: f64,
    eta_d: f64,
    site_cap_kw: f64,
) -> f64 {
    let v = view.vehicle;
    if sp.power_kw >= 0.0 {
        // Charging: grid-side power, battery gains eta_c fraction.
        let mut p = sp.power_kw.min(view.max_charge_kw).min(site_cap_kw);
        // Don't overfill the battery.
        let room_kwh = v.ceiling_kwh() - session.soc_kwh;
        let max_grid_kwh = if eta_c > 0.0 { room_kwh / eta_c } else { 0.0 };
        p = p.min(kwh_to_kw(max_grid_kwh, dt_min)).max(0.0);
        let grid_kwh = kw_to_kwh(p, dt_min);
        session.soc_kwh += grid_kwh * eta_c;
        session.energy_drawn_kwh += grid_kwh;
        p
    } else {
        // Discharging: building-side power, battery loses 1/eta_d per unit.
        // Bounded by the port/vehicle limit, the SoC floor, and no-export.
        let mut p = (-sp.power_kw).min(view.max_discharge_kw).min(site_cap_kw);
        let max_building_kwh = (session.soc_kwh - v.min_soc_kwh).max(0.0) * eta_d;
        p = p.min(kwh_to_kw(max_building_kwh, dt_min)).max(0.0);
        let building_kwh = kw_to_kwh(p, dt_min);
        session.soc_kwh -= building_kwh / eta_d;
        session.energy_exported_kwh += building_kwh;
        -p
    }
}

/// A placeholder session for a vehicle that never obtained a charger.
fn unserved_session(vehicle_index: usize, arrival_soc_kwh: f64) -> Session {
    Session {
        vehicle_index,
        charger_index: 0,
        soc_arrival_kwh: arrival_soc_kwh,
        soc_kwh: arrival_soc_kwh,
        energy_drawn_kwh: 0.0,
        energy_exported_kwh: 0.0,
    }
}

fn finish(
    v: &Vehicle,
    s: &Session,
    chain_clamped_kwh: f64,
    never_connected: bool,
) -> SessionResult {
    SessionResult {
        vehicle_id: v.vehicle_id,
        arrival_slot: v.arrival_slot,
        departure_slot: v.departure_slot,
        soc_arrival_kwh: s.soc_arrival_kwh,
        soc_departure_kwh: s.soc_kwh,
        soc_target_kwh: v.soc_target_kwh,
        // target_met is derived from missing_kwh so the two can never
        // contradict; the 1e-9 band absorbs third-party floating-point dust.
        target_met: (v.soc_target_kwh - s.soc_kwh).max(0.0) <= 1e-9,
        missing_kwh: (v.soc_target_kwh - s.soc_kwh).max(0.0),
        banked_kwh: (s.soc_kwh - v.soc_target_kwh).max(0.0),
        energy_drawn_kwh: s.energy_drawn_kwh,
        energy_exported_kwh: s.energy_exported_kwh,
        chain_clamped_kwh,
        never_connected,
    }
}
