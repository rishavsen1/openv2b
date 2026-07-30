//! First-principles invariant tests. These hold for EVERY policy, because the
//! engine (not the policy) enforces physics. Each test states the invariant it
//! pins; see docs/SPEC.md section "Invariants".

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, dr_event, vehicle};
use openv2b::engine::run;
use openv2b::policy::{self, POLICY_NAMES};
use openv2b::scenario::Scenario;

fn all_policies() -> impl Iterator<Item = Box<dyn policy::Policy>> {
    POLICY_NAMES
        .iter()
        .map(|n| policy::by_name(n).expect("registered policy"))
}

/// A scenario that exercises charging, discharging, DR, and charger contention.
fn rich_scenario() -> Scenario {
    let mut v1 = vehicle(0, 0, 40);
    v1.soc_arrival_kwh = 50.0; // surplus vehicle: can bank/discharge
    v1.soc_target_kwh = 30.0;
    let mut v2 = vehicle(1, 4, 30);
    v2.soc_arrival_kwh = 5.0; // deficit vehicle
    v2.soc_target_kwh = 45.0;
    let v3 = vehicle(2, 8, 20); // contends for chargers
    let mut s = base_scenario(
        48,
        vec![v1, v2, v3],
        vec![charger(0, true), charger(1, false)],
    );
    s.dr_events.push(dr_event(12, 20, 40.0));
    s
}

/// Invariant 1: per-session battery energy balance. The SoC change equals
/// charge_efficiency * energy_drawn - energy_exported / discharge_efficiency.
#[test]
fn energy_conservation_per_session() {
    for pol in all_policies() {
        let mut scenario = rich_scenario();
        scenario.manifest.charge_efficiency = 0.9;
        scenario.manifest.discharge_efficiency = 0.95;
        let results = run(&scenario, pol.as_ref());
        for s in &results.sessions {
            let expected =
                s.soc_arrival_kwh + 0.9 * s.energy_drawn_kwh - s.energy_exported_kwh / 0.95;
            assert_abs_diff_eq!(s.soc_departure_kwh, expected, epsilon = 1e-9);
        }
    }
}

/// Invariant 2: SoC never leaves [min_soc, battery capacity]. Verified at the
/// only observable point (departure) plus by the export/draw accounting, which
/// the engine clamps against the same bounds every slot.
#[test]
fn soc_bounds_respected() {
    for pol in all_policies() {
        let mut scenario = rich_scenario();
        for v in &mut scenario.vehicles {
            v.min_soc_kwh = 10.0;
            v.soc_arrival_kwh = v.soc_arrival_kwh.max(10.0);
        }
        let results = run(&scenario, pol.as_ref());
        for s in &results.sessions {
            let v = scenario
                .vehicles
                .iter()
                .find(|v| v.vehicle_id == s.vehicle_id && v.arrival_slot == s.arrival_slot)
                .expect("session belongs to a vehicle");
            assert!(
                s.soc_departure_kwh <= v.battery_kwh + 1e-9,
                "policy {}: vehicle {} overfilled",
                results.policy,
                s.vehicle_id
            );
            assert!(
                s.soc_departure_kwh >= v.min_soc_kwh - 1e-9,
                "policy {}: vehicle {} below SoC floor",
                results.policy,
                s.vehicle_id
            );
        }
    }
}

/// Invariant 3 (no export): net site load never goes negative, for any policy,
/// even with huge discharge capability and near-zero building load.
#[test]
fn no_export_to_grid() {
    for pol in all_policies() {
        let mut v = vehicle(0, 0, 40);
        v.soc_arrival_kwh = 60.0;
        v.soc_target_kwh = 0.0;
        v.max_discharge_kw = 500.0;
        let mut scenario = base_scenario(48, vec![v], vec![charger(0, true)]);
        scenario.building_load_kw = vec![1.0; 48];
        scenario.dr_events.push(dr_event(0, 47, 0.0)); // invite maximal discharge
        let results = run(&scenario, pol.as_ref());
        for rec in &results.slots {
            assert!(
                rec.net_kw >= -1e-9,
                "policy {}: slot {} exported {} kW",
                results.policy,
                rec.slot,
                -rec.net_kw
            );
        }
    }
}

/// Invariant 4: aggregate charging never exceeds the fleet's physical limit
/// (each session capped by min(vehicle, charger port)).
#[test]
fn power_caps_respected() {
    for pol in all_policies() {
        let scenario = rich_scenario();
        let results = run(&scenario, pol.as_ref());
        // Two chargers at 20 kW each bound total charge and discharge.
        for rec in &results.slots {
            assert!(
                rec.ev_charge_kw <= 40.0 + 1e-9,
                "policy {}: charge over cap",
                results.policy
            );
            assert!(
                rec.ev_discharge_kw <= 40.0 + 1e-9,
                "policy {}: discharge over cap",
                results.policy
            );
        }
    }
}

/// Invariant 5: determinism. Two runs of the same scenario/policy serialize
/// byte-identically.
#[test]
fn determinism_byte_identical() {
    for pol in all_policies() {
        let scenario = rich_scenario();
        let a = serde_json::to_string(&run(&scenario, pol.as_ref())).expect("results serialize");
        let b = serde_json::to_string(&run(&scenario, pol.as_ref())).expect("results serialize");
        assert_eq!(a, b, "policy {} is nondeterministic", pol.name());
    }
}

/// Invariant 6: bill identity. total = energy + demand + penalty - incentive,
/// and the DR overflow is the (start, end] windowed positive part times slot hours.
#[test]
fn bill_identity_and_overflow_definition() {
    for pol in all_policies() {
        let mut scenario = rich_scenario();
        scenario.manifest.demand_charge_usd_per_kw = 11.67;
        let results = run(&scenario, pol.as_ref());
        let b = &results.bill;
        assert_abs_diff_eq!(
            b.total_usd,
            b.energy_usd + b.demand_usd + b.dr_penalty_usd - b.dr_incentive_usd,
            epsilon = 1e-9
        );
        let event = &scenario.dr_events[0];
        let expected_overflow: f64 = results
            .slots
            .iter()
            .filter(|r| r.slot > event.start_slot && r.slot <= event.end_slot)
            .map(|r| (r.net_kw - event.fsl_kw).max(0.0) * 0.25)
            .sum();
        assert_abs_diff_eq!(
            b.dr_settlements[0].overflow_kwh,
            expected_overflow,
            epsilon = 1e-9
        );
    }
}

/// Departure guarantee: with ample time and no contention, every policy that
/// charges toward the target actually reaches it.
#[test]
fn feasible_targets_are_met() {
    for name in ["uncontrolled", "edf", "llf", "edf-v2b", "llf-v2b"] {
        let pol = policy::by_name(name).expect("registered policy");
        // Need 20 kWh at up to 20 kW: 4 slots suffice; give 40.
        let scenario = base_scenario(48, vec![vehicle(0, 0, 40)], vec![charger(0, true)]);
        let results = run(&scenario, pol.as_ref());
        assert!(
            results.sessions[0].target_met,
            "policy {name} missed a trivially feasible target"
        );
    }
}

/// Charger contention: with one charger and two overlapping sessions, the
/// second vehicle connects only after the first departs, and is flagged
/// never_connected if it cannot.
#[test]
fn charger_queueing_is_deterministic_and_fair() {
    let v1 = vehicle(0, 0, 10);
    let v2 = vehicle(1, 2, 30); // waits until slot 10
    let v3 = vehicle(2, 2, 8); // departs before a charger frees: never connects
    let scenario = base_scenario(48, vec![v1, v2, v3], vec![charger(0, false)]);
    let results = run(
        &scenario,
        policy::by_name("edf").expect("edf exists").as_ref(),
    );
    let by_id = |id: u32| {
        results
            .sessions
            .iter()
            .find(|s| s.vehicle_id == id)
            .expect("every vehicle produces a session result")
    };
    assert!(!by_id(0).never_connected, "first vehicle must connect");
    assert!(
        !by_id(1).never_connected,
        "waiting vehicle must connect after departure"
    );
    assert!(
        by_id(2).never_connected,
        "overlapping vehicle must be reported as unserved"
    );
    assert_abs_diff_eq!(
        by_id(2).soc_departure_kwh,
        by_id(2).soc_arrival_kwh,
        epsilon = 1e-12
    );
}

/// V2B effectiveness: during a DR window whose firm level is below the building
/// load, the V2B variant strictly reduces overflow (and the bill) relative to
/// the charge-only variant.
#[test]
fn v2b_reduces_dr_overflow() {
    let build = || {
        let mut v = vehicle(0, 0, 48);
        v.soc_arrival_kwh = 55.0;
        v.soc_target_kwh = 20.0;
        let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 48];
        s.dr_events.push(dr_event(10, 20, 40.0)); // 10 kW shortfall for 10 slots
        s
    };
    let edf = run(
        &build(),
        policy::by_name("edf").expect("edf exists").as_ref(),
    );
    let edf_v2b = run(
        &build(),
        policy::by_name("edf-v2b").expect("edf-v2b exists").as_ref(),
    );
    assert!(
        edf.bill.dr_penalty_usd > 0.0,
        "charge-only baseline must overflow"
    );
    assert!(
        edf_v2b.bill.dr_penalty_usd < edf.bill.dr_penalty_usd,
        "V2B must shave DR overflow: {} vs {}",
        edf_v2b.bill.dr_penalty_usd,
        edf.bill.dr_penalty_usd
    );
    assert!(
        edf_v2b.bill.total_usd < edf.bill.total_usd,
        "V2B must lower the bill here"
    );
}

/// The DR window convention is half-open on the left: (start, end].
#[test]
fn dr_window_is_left_open_right_closed() {
    let e = dr_event(4, 8, 10.0);
    assert!(!e.contains(4), "start slot must be excluded");
    assert!(e.contains(5), "first slot after start must be included");
    assert!(e.contains(8), "end slot must be included");
    assert!(!e.contains(9), "slot after end must be excluded");
}

/// Discharge budget safety: a V2B policy must never discharge energy it cannot
/// recover before departure. The vehicle ends at (or above) its target even
/// after serving a DR window mid-session.
#[test]
fn v2b_discharge_never_sacrifices_departure_target() {
    let mut v = vehicle(0, 0, 24);
    v.soc_arrival_kwh = 45.0;
    v.soc_target_kwh = 40.0;
    let mut scenario = base_scenario(48, vec![v], vec![charger(0, true)]);
    scenario.dr_events.push(dr_event(2, 10, 30.0));
    for name in ["edf-v2b", "llf-v2b"] {
        let results = run(
            &scenario,
            policy::by_name(name).expect("registered").as_ref(),
        );
        assert!(
            results.sessions[0].target_met,
            "policy {name} discharged past the recoverable budget"
        );
    }
}
