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
/// byte-identically. Policies are stateful per episode (the EDF/LLF ratchet,
/// mirroring the reference's per-episode instances), so each run gets a
/// FRESH instance, exactly as each reference episode constructs its own.
#[test]
fn determinism_byte_identical() {
    for name in POLICY_NAMES {
        let scenario = rich_scenario();
        let a = serde_json::to_string(&run(
            &scenario,
            policy::by_name(name).expect("registered").as_ref(),
        ))
        .expect("results serialize");
        let b = serde_json::to_string(&run(
            &scenario,
            policy::by_name(name).expect("registered").as_ref(),
        ))
        .expect("results serialize");
        assert_eq!(a, b, "policy {name} is nondeterministic");
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

/// POLICY_1 is the reference's explicit discharge channel: at peak TOU it
/// discharges above-target cars at the charger's full rate, reducing net
/// load below the building baseline.
#[test]
fn policy1_discharges_above_target_cars_at_peak() {
    let mut v = vehicle(0, 0, 48);
    v.soc_arrival_kwh = 55.0;
    v.soc_target_kwh = 20.0;
    let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 48];
    for slot in 10..20 {
        s.tou_class[slot] = openv2b::scenario::TouClass::Peak;
    }
    let r = run(
        &s,
        policy::by_name("policy-1").expect("registered").as_ref(),
    );
    let peak_discharge: f64 = r.slots[10..20].iter().map(|x| x.ev_discharge_kw).sum();
    assert!(
        peak_discharge > 0.0,
        "policy-1 must discharge at peak (got {peak_discharge})"
    );
    for rec in &r.slots[10..20] {
        assert!(
            rec.net_kw <= 50.0 + 1e-9,
            "discharge must reduce net below building"
        );
    }
}

/// The reference's force-charge discharge channel: within the final hour, an
/// above-target car is discharged at exactly the metered rate that lands it
/// on the target at departure.
#[test]
fn force_charge_meters_discharge_onto_the_target() {
    let mut v = vehicle(0, 0, 24);
    v.soc_arrival_kwh = 45.0;
    v.soc_target_kwh = 40.0;
    let mut scenario = base_scenario(48, vec![v], vec![charger(0, true)]);
    scenario.manifest.heuristic_threshold_kw = Some(100.0);
    for name in ["edf", "llf"] {
        let results = run(
            &scenario,
            policy::by_name(name).expect("registered").as_ref(),
        );
        let s = &results.sessions[0];
        assert!(s.target_met, "policy {name}: target missed");
        assert_abs_diff_eq!(s.soc_departure_kwh, 40.0, epsilon = 1e-9);
        assert!(
            s.energy_exported_kwh > 0.0,
            "policy {name}: the 5 kWh surplus must be discharged in the final hour"
        );
    }
}

/// Departure guarantee: with ample time, budget headroom, and no contention,
/// the target-seeking policies reach the target. (policy-1/2 are TOU-gated
/// chargers with no such guarantee; edf/llf need the threshold above the
/// building load or only the 1-hour force-charge window serves.)
#[test]
fn feasible_targets_are_met() {
    for name in ["uncontrolled", "policy-0", "edf", "llf"] {
        let pol = policy::by_name(name).expect("registered policy");
        // Need 20 kWh at up to 20 kW: 4 slots suffice; give 40.
        let mut scenario = base_scenario(48, vec![vehicle(0, 0, 40)], vec![charger(0, true)]);
        scenario.manifest.heuristic_threshold_kw = Some(100.0); // headroom over the 50 kW building
        let results = run(&scenario, pol.as_ref());
        assert!(
            results.sessions[0].target_met,
            "policy {name} missed a trivially feasible target"
        );
    }
}

/// Charger contention, reference semantics: a car that finds no vacant port
/// at ARRIVAL is dropped permanently (never retried), even if a port frees
/// later; lower vehicle ids win same-slot contention.
#[test]
fn charger_assignment_reference_semantics() {
    let v1 = vehicle(0, 0, 10);
    let v2 = vehicle(1, 2, 30); // arrives while the port is held: dropped forever
    let v3 = vehicle(2, 2, 8); // likewise
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
        by_id(1).never_connected,
        "reference semantics: no retry after the port frees at slot 10"
    );
    assert!(by_id(2).never_connected, "dropped at arrival");
    assert_abs_diff_eq!(
        by_id(2).soc_departure_kwh,
        by_id(2).soc_arrival_kwh,
        epsilon = 1e-12
    );
    assert_eq!(
        results.sessions.len(),
        3,
        "dropped sessions reported exactly once"
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
