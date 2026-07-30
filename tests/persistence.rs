//! Persistence (cross-day SoC chaining) and TOU billing tests.

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, vehicle};
use openv2b::engine::run;
use openv2b::policy::{self, Policy};
use openv2b::scenario::{Scenario, TouClass, Vehicle};
use openv2b::state::{Observation, Setpoint};

/// A banking policy: charge every session at full power all the time.
struct Greedy;

impl Policy for Greedy {
    fn name(&self) -> &'static str {
        "greedy-test"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: s.max_charge_kw,
            })
            .collect()
    }
}

/// Three sessions of one vehicle across a 3-day horizon (96 slots/day).
fn commuter_sessions() -> Vec<Vehicle> {
    let mk = |arr: usize, dep: usize, depletion: f64| {
        let mut v = vehicle(7, arr, dep);
        v.battery_kwh = 60.0;
        v.soc_arrival_kwh = 20.0; // only meaningful for the first session
        v.soc_target_kwh = 40.0;
        v.max_charge_kw = 20.0;
        v.depletion_kwh = depletion;
        v
    };
    vec![
        mk(36, 68, 0.0),              // day 1
        mk(36 + 96, 68 + 96, 12.0),   // day 2: drove 12 kWh in between
        mk(36 + 192, 68 + 192, 12.0), // day 3
    ]
}

/// P15: arrival SoC of session k+1 = clamp(departure SoC of k - depletion).
#[test]
fn chain_identity_under_uncontrolled() {
    let scenario = base_scenario(288, commuter_sessions(), vec![charger(0, true)]);
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    assert_eq!(results.sessions.len(), 3, "three chained sessions");
    // Uncontrolled charges each session exactly to its 40 kWh target.
    assert_abs_diff_eq!(results.sessions[0].soc_arrival_kwh, 20.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.sessions[0].soc_departure_kwh, 40.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.sessions[1].soc_arrival_kwh, 28.0, epsilon = 1e-9); // 40 - 12
    assert_abs_diff_eq!(results.sessions[1].soc_departure_kwh, 40.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.sessions[2].soc_arrival_kwh, 28.0, epsilon = 1e-9);
}

/// P16: banked surplus survives to the next session and reduces grid energy
/// needed there by exactly the surviving surplus (lossless case).
#[test]
fn banked_energy_reduces_next_session_draw() {
    let scenario = base_scenario(288, commuter_sessions(), vec![charger(0, true)]);
    let greedy = run(&scenario, &Greedy);
    // Greedy fills to capacity (60): banked = 60 - 40 = 20 kWh per session.
    assert_abs_diff_eq!(greedy.sessions[0].soc_departure_kwh, 60.0, epsilon = 1e-9);
    assert_abs_diff_eq!(greedy.sessions[0].banked_kwh, 20.0, epsilon = 1e-9);
    // Session 2 arrives at 60 - 12 = 48 instead of 28: 20 kWh less to draw
    // to reach capacity again.
    assert_abs_diff_eq!(greedy.sessions[1].soc_arrival_kwh, 48.0, epsilon = 1e-9);
    assert_abs_diff_eq!(
        greedy.sessions[1].energy_drawn_kwh,
        60.0 - 48.0,
        epsilon = 1e-9
    );
}

/// P17: persistence off makes every session use its CSV arrival SoC.
#[test]
fn persistence_off_restores_independent_sessions() {
    let mut scenario = base_scenario(288, commuter_sessions(), vec![charger(0, true)]);
    scenario.manifest.persistence = false;
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    for s in &results.sessions {
        assert_abs_diff_eq!(s.soc_arrival_kwh, 20.0, epsilon = 1e-9);
    }
}

/// Depletion larger than the previous departure SoC clamps at the floor.
#[test]
fn depletion_clamps_at_floor() {
    let mut sessions = commuter_sessions();
    sessions[1].depletion_kwh = 500.0;
    for s in &mut sessions {
        s.min_soc_kwh = 5.0;
    }
    let scenario = base_scenario(288, sessions, vec![charger(0, true)]);
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    assert_abs_diff_eq!(results.sessions[1].soc_arrival_kwh, 5.0, epsilon = 1e-9);
    // The declared trip was infeasible (drove 500 kWh on a 40 kWh departure);
    // the manufactured energy is reported, never silent: 5 - (40 - 500) = 465.
    assert_abs_diff_eq!(results.sessions[1].chain_clamped_kwh, 465.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.sessions[0].chain_clamped_kwh, 0.0, epsilon = 1e-9);
}

/// A same-slot handoff (session k departs slot s, session k+1 arrives slot s)
/// chains correctly because departures are processed before arrivals.
#[test]
fn same_slot_handoff_chains() {
    let mut sessions = commuter_sessions();
    sessions[1].arrival_slot = sessions[0].departure_slot;
    sessions[1].departure_slot = sessions[0].departure_slot + 32;
    let scenario = base_scenario(288, sessions, vec![charger(0, true)]);
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    assert_abs_diff_eq!(results.sessions[1].soc_arrival_kwh, 28.0, epsilon = 1e-9);
}

/// An unserved chained session still propagates its (unchanged) SoC forward.
#[test]
fn unserved_session_propagates_chain() {
    let mut sessions = commuter_sessions();
    // A blocker occupies the only charger for all of day 2.
    let mut blocker = vehicle(99, 96, 192);
    blocker.soc_target_kwh = 60.0;
    sessions.push(blocker);
    let scenario = base_scenario(288, sessions, vec![charger(0, true)]);
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    let day2 = results
        .sessions
        .iter()
        .find(|s| s.vehicle_id == 7 && s.arrival_slot == 132)
        .expect("day-2 session exists");
    assert!(day2.never_connected, "day-2 session must be unserved");
    assert_abs_diff_eq!(day2.soc_arrival_kwh, 28.0, epsilon = 1e-9);
    let day3 = results
        .sessions
        .iter()
        .find(|s| s.vehicle_id == 7 && s.arrival_slot == 228)
        .expect("day-3 session exists");
    // Chain continues from the unserved session's SoC: 28 - 12 = 16.
    assert_abs_diff_eq!(day3.soc_arrival_kwh, 16.0, epsilon = 1e-9);
}

/// Overlapping sessions of one vehicle are rejected at validation.
#[test]
fn overlapping_sessions_rejected() {
    let mut sessions = commuter_sessions();
    sessions[1].arrival_slot = sessions[0].departure_slot - 1;
    let scenario = Scenario {
        vehicles: sessions,
        ..base_scenario(288, vec![], vec![charger(0, true)])
    };
    assert!(scenario.validate().is_err(), "overlap must be rejected");
}

/// TOU demand charge: time-related component bills the peak-class maximum,
/// facilities component bills the overall maximum. Hand-computed.
#[test]
fn tou_demand_charge_hand_computed() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 40.0; // 20 kWh at 20 kW: slots 0-3 charge
    let mut scenario = base_scenario(16, vec![v], vec![charger(0, false)]);
    scenario.building_load_kw = vec![10.0; 16];
    // Slots 8..16 are peak class, but the EV is gone by then: peak-class max
    // net = 10 kW, overall max = 30 kW (slot 0, off-peak).
    for s in 8..16 {
        scenario.tou_class[s] = TouClass::Peak;
    }
    scenario.manifest.demand_charge_usd_per_kw = 2.0;
    scenario.manifest.demand_charge_peak_usd_per_kw = 11.67;
    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    assert_abs_diff_eq!(results.bill.peak_net_kw, 30.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.peak_net_peak_tou_kw, 10.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.demand_facilities_usd, 60.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.demand_peak_tou_usd, 116.70, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.demand_usd, 176.70, epsilon = 1e-9);
}
