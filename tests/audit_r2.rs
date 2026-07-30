//! Regression tests for the R2 correctness-audit findings (emission-order
//! arbitration, force-charge under DR, validation hardening).

mod common;

use common::{base_scenario, charger, dr_event, manifest, vehicle};
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
        let pol = policy::by_name(name).expect("registered");
        let a = run(&mk(&[7, 9]), pol.as_ref());
        let b = run(&mk(&[9, 7]), pol.as_ref());
        assert_eq!(
            serde_json::to_string(&a.sessions).expect("serialize"),
            serde_json::to_string(&b.sessions).expect("serialize"),
            "policy {name}: row order changed outcomes under a binding cap"
        );
    }
}

/// F6: a trivially feasible target inside a DR window is met by every
/// built-in policy (force-charge fallback): the firm level yields to the
/// service guarantee, at the cost of a window penalty.
#[test]
fn feasible_target_met_even_when_building_exceeds_firm_level() {
    let build = || {
        let mut v = vehicle(0, 0, 8);
        v.soc_arrival_kwh = 20.0;
        v.soc_target_kwh = 40.0; // 1 h at 20 kW, 2 h available
        let mut s = base_scenario(8, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 8];
        s.dr_events.push(dr_event(0, 7, 40.0)); // building alone violates F
        s
    };
    for name in ["uncontrolled", "edf", "llf", "edf-v2b", "llf-v2b"] {
        let r = run(
            &build(),
            policy::by_name(name).expect("registered").as_ref(),
        );
        assert!(
            r.sessions[0].target_met,
            "{name}: missed a trivially feasible target inside a DR window (SoC {})",
            r.sessions[0].soc_departure_kwh
        );
    }
    // The priority policies pay for it: the forced charging shows up as
    // window penalty beyond the building's own overflow.
    let edf = run(
        &build(),
        policy::by_name("edf").expect("registered").as_ref(),
    );
    let idle = run(
        &build(),
        policy::by_name("idle").expect("registered").as_ref(),
    );
    assert!(
        edf.bill.dr_penalty_usd > idle.bill.dr_penalty_usd,
        "force-charge must show up as extra window penalty"
    );
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

/// F8 follow-through: V2B banking. Outside peak-price slots the V2B variants
/// charge above the target, so a chained donor regains its surplus daily
/// instead of running dry after the first window.
#[test]
fn v2b_banking_replenishes_the_donor() {
    // Two days; a window each day; donor present both days.
    let mk_day = |day: usize| {
        let mut v = vehicle(2, day * 96 + 40, day * 96 + 80);
        v.battery_kwh = 100.0;
        v.soc_arrival_kwh = 90.0; // only meaningful on day 0
        v.soc_target_kwh = 40.0;
        v.min_soc_kwh = 10.0;
        v.depletion_kwh = 5.0;
        v
    };
    let mut s = base_scenario(192, vec![mk_day(0), mk_day(1)], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 192];
    s.dr_events.push(dr_event(60, 70, 40.0));
    s.dr_events.push(dr_event(96 + 60, 96 + 70, 40.0));
    let r = run(&s, policy::by_name("edf-v2b").expect("registered").as_ref());
    let day2 = &r.sessions[1];
    assert!(
        day2.energy_exported_kwh > 0.0,
        "banked donor must still discharge on day 2 (exported {})",
        day2.energy_exported_kwh
    );
    assert!(r.bill.dr_penalty_usd < 1e-6, "both windows fully covered");
}
