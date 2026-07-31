//! Regression tests for the R2 correctness-audit findings (emission-order
//! arbitration, force-charge under DR, validation hardening).

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, manifest, vehicle};
use openv2b::engine::run;
use openv2b::policy::{self, Policy, POLICY_NAMES};
use openv2b::scenario::{Scenario, TouClass};
use openv2b::state::{Observation, Setpoint};

/// F1: scarce headroom is rationed in the POLICY'S emission order, not CSV
/// row order. Two identical vehicles, 10 kW of site-cap headroom; a policy
/// that emits (session B first, session A second) must feed B.
struct EmitReversed;

impl Policy for EmitReversed {
    fn name(&self) -> &'static str {
        "emit-reversed-test"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut sp: Vec<Setpoint> = obs
            .sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: 20.0,
            })
            .collect();
        sp.reverse();
        sp
    }
}

#[test]
fn headroom_rationed_in_emission_order() {
    let mut s = base_scenario(
        4,
        vec![vehicle(7, 0, 4), vehicle(9, 0, 4)],
        vec![charger(0, false), charger(1, false)],
    );
    s.building_load_kw = vec![50.0; 4];
    s.manifest.site_cap_kw = Some(60.0); // 10 kW headroom
    let r = run(&s, &EmitReversed);
    // Canonical session order is (arrival, vehicle_id): views [7, 9];
    // reversed emission puts vehicle 9 first, so 9 gets the 10 kW.
    let by_id = |id: u32| {
        r.sessions
            .iter()
            .find(|x| x.vehicle_id == id)
            .expect("session present")
            .energy_drawn_kwh
    };
    assert!(
        by_id(9) > by_id(7),
        "emission order must win: 9 was asked first"
    );
}

/// F2: with a BINDING site cap, permuting vehicle CSV rows must not change
/// any outcome (the pre-fix engine rationed in row order and failed this).
#[test]
fn row_permutation_invariance_under_binding_cap() {
    let mk = |order: &[u32]| {
        let vehicles = order.iter().map(|&id| vehicle(id, 0, 4)).collect();
        let mut s = base_scenario(4, vehicles, vec![charger(0, false), charger(1, false)]);
        s.building_load_kw = vec![50.0; 4];
        s.manifest.site_cap_kw = Some(60.0);
        s
    };
    for name in POLICY_NAMES {
        // Fresh instance per run: the EDF/LLF ratchet is per-episode state.
        let a = run(
            &mk(&[7, 9]),
            policy::by_name(name).expect("registered").as_ref(),
        );
        let b = run(
            &mk(&[9, 7]),
            policy::by_name(name).expect("registered").as_ref(),
        );
        assert_eq!(
            serde_json::to_string(&a.sessions).expect("serialize"),
            serde_json::to_string(&b.sessions).expect("serialize"),
            "policy {name}: row order changed outcomes under a binding cap"
        );
    }
}

/// F5: non-finite inputs are rejected at validation, never repaired into a
/// plausible under-stated bill.
#[test]
fn non_finite_inputs_rejected() {
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.building_load_kw[2] = f64::NAN;
    assert!(s.validate().is_err(), "NaN building load must be rejected");

    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.price_usd_per_kwh[3] = f64::INFINITY;
    assert!(s.validate().is_err(), "infinite price must be rejected");

    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.vehicles[0].battery_kwh = f64::INFINITY;
    assert!(s.validate().is_err(), "infinite battery must be rejected");
}

/// F9: out-of-range vehicle fields and duplicate charger ids are rejected.
#[test]
fn range_and_uniqueness_validation() {
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.vehicles[0].min_soc_kwh = 40.0;
    s.vehicles[0].soc_arrival_kwh = 5.0; // arrives below its own floor
    assert!(
        s.validate().is_err(),
        "arrival below floor must be rejected"
    );

    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.vehicles[0].soc_target_kwh = -1000.0;
    assert!(s.validate().is_err(), "negative target must be rejected");

    let s = Scenario {
        manifest: manifest(8),
        vehicles: vec![vehicle(0, 0, 8)],
        chargers: vec![charger(3, true), charger(3, false)],
        building_load_kw: vec![10.0; 8],
        price_usd_per_kwh: vec![0.2; 8],
        tou_class: vec![TouClass::OffPeak; 8],
        dr_events: vec![],
    };
    assert!(
        s.validate().is_err(),
        "duplicate charger_id must be rejected"
    );
}

/// Reference force-charge: with the budget dead (threshold fallback 0.8*max
/// building sits below the load), nothing charges until the final hour, then
/// the metered force-charge lands the car exactly on a small target.
#[test]
fn force_charge_serves_within_final_hour_despite_dead_budget() {
    for name in ["edf", "llf"] {
        let mut v = vehicle(0, 0, 8);
        v.soc_arrival_kwh = 35.0;
        v.soc_target_kwh = 40.0; // 5 kWh: coverable by the 3-slot force window
        let mut s = base_scenario(8, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 8]; // fallback threshold = 40 < 50
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        let early: f64 = r.slots[..5].iter().map(|x| x.ev_charge_kw).sum();
        assert_abs_diff_eq!(early, 0.0, epsilon = 1e-9);
        assert!(
            r.sessions[0].target_met,
            "{name}: force-charge must land the target"
        );
        assert_abs_diff_eq!(r.sessions[0].soc_departure_kwh, 40.0, epsilon = 1e-9);
    }
}
