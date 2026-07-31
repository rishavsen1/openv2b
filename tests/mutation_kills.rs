//! Tests closing the R2 mutation-audit survivors (M5, M6, M10, M12, M13,
//! M14, B1, B3). Each test names its mutant and is designed to FAIL under
//! that mutation; the geometries avoid the "true for the wrong reason"
//! failure mode the audit identified (e.g. properties that held only because
//! a policy self-limited before the engine guard could bind).

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, dr_event, manifest, vehicle};
use openv2b::engine::run;
use openv2b::policy::{self, Policy};
use openv2b::scenario::{Scenario, TouClass};
use openv2b::state::{Observation, Setpoint};

/// Requests maximum discharge from every session, unconditionally. Unlike the
/// V2B heuristics (which self-limit to the FSL excess), this makes the
/// engine's no-export guard the ONLY thing standing between the battery and
/// the grid.
struct MaxDischarge;

impl Policy for MaxDischarge {
    fn name(&self) -> &'static str {
        "max-discharge-test"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: -1e15,
            })
            .collect()
    }
}

/// M5: the no-export guard must BIND: tiny building load, huge discharge
/// capability, and a policy that asks for everything. Discharge is clamped to
/// exactly the building load; net stays at zero.
#[test]
fn no_export_guard_binds_under_max_discharge() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 60.0;
    v.soc_target_kwh = 0.0;
    v.max_discharge_kw = 500.0;
    let mut ch = charger(0, true);
    ch.max_kw = 500.0;
    let mut s = base_scenario(8, vec![v], vec![ch]);
    s.building_load_kw = vec![0.5; 8];
    let r = run(&s, &MaxDischarge);
    for rec in &r.slots {
        assert!(
            rec.net_kw >= -1e-9,
            "exported {} kW at slot {}",
            -rec.net_kw,
            rec.slot
        );
        assert!(
            rec.ev_discharge_kw <= 0.5 + 1e-9,
            "discharge {} exceeds the 0.5 kW building load",
            rec.ev_discharge_kw
        );
    }
    assert_abs_diff_eq!(r.slots[0].net_kw, 0.0, epsilon = 1e-9);
}

/// M6: `run()` never calls `validate()`, so the billing-side covered-slot
/// gate is reachable via a directly constructed (unvalidated) scenario. An
/// event with zero simulated slots must pay zero incentive.
#[test]
fn unvalidated_out_of_horizon_event_pays_no_incentive() {
    let mut e = dr_event(100, 200, 5.0);
    e.baseline_kw = 100.0;
    let scenario = Scenario {
        manifest: manifest(8),
        vehicles: vec![vehicle(0, 0, 8)],
        chargers: vec![charger(0, true)],
        building_load_kw: vec![50.0; 8],
        price_usd_per_kwh: vec![0.20; 8],
        tou_class: vec![TouClass::OffPeak; 8],
        dr_events: vec![e],
    };
    let r = run(
        &scenario,
        policy::by_name("edf").expect("registered").as_ref(),
    );
    assert_abs_diff_eq!(r.bill.dr_incentive_usd, 0.0, epsilon = 1e-12);
    assert!(r.bill.total_usd > 0.0, "no free negative bill");
}

/// M14: the 1e-9 target tolerance is load-bearing for third-party policies
/// that land within floating-point dust of the target.
struct AlmostExact;

impl Policy for AlmostExact {
    fn name(&self) -> &'static str {
        "almost-exact-test"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: if s.remaining_need_kwh() > 1e-6 {
                    80.0 - 4e-12
                } else {
                    0.0
                },
            })
            .collect()
    }
}

#[test]
fn target_met_tolerance_absorbs_floating_point_dust() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 40.0;
    v.max_charge_kw = 100.0;
    let mut ch = charger(0, false);
    ch.max_kw = 100.0;
    let s = base_scenario(8, vec![v], vec![ch]);
    let r = run(&s, &AlmostExact);
    // One slot at (80 - 4e-12) kW = 20 kWh minus ~1e-12: within the 1e-9 band.
    assert!(
        r.sessions[0].soc_departure_kwh < 40.0,
        "geometry check: SoC must land strictly below the target"
    );
    assert!(
        40.0 - r.sessions[0].soc_departure_kwh < 1e-9,
        "geometry check: within the tolerance band"
    );
    assert!(
        r.sessions[0].target_met,
        "dust-sized shortfall counts as met"
    );
}

/// B1: the incentive pays on the committed REDUCTION (baseline - fsl), gated
/// on honoring the window, never on the full baseline.
#[test]
fn incentive_pays_reduction_only_and_only_when_honored() {
    // Honored window: building 30 <= fsl 40.
    let mut s = base_scenario(16, vec![vehicle(0, 0, 8)], vec![charger(0, false)]);
    s.building_load_kw = vec![30.0; 16];
    let mut honored = dr_event(8, 12, 40.0); // after the EV departs
    honored.baseline_kw = 50.0;
    s.dr_events.push(honored);
    let r = run(&s, policy::by_name("edf").expect("registered").as_ref());
    assert_abs_diff_eq!(r.bill.dr_penalty_usd, 0.0, epsilon = 1e-9);
    // 13.6 $/kW * (50 - 40) kW = $136.00; paying on the baseline would be $680.
    assert_abs_diff_eq!(r.bill.dr_incentive_usd, 136.0, epsilon = 1e-9);

    // Violated window: building 45 > fsl 40 -> penalty, zero incentive.
    let mut s = base_scenario(16, vec![vehicle(0, 0, 8)], vec![charger(0, false)]);
    s.building_load_kw = vec![45.0; 16];
    let mut violated = dr_event(8, 12, 40.0);
    violated.baseline_kw = 50.0;
    s.dr_events.push(violated);
    let r = run(&s, policy::by_name("edf").expect("registered").as_ref());
    assert!(
        r.bill.dr_penalty_usd > 0.0,
        "violated window must be penalized"
    );
    assert_abs_diff_eq!(r.bill.dr_incentive_usd, 0.0, epsilon = 1e-12);
}

/// B3: a unidirectional port never discharges, even under a policy that
/// demands it and a vehicle that could.
#[test]
fn unidirectional_port_never_discharges() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 60.0;
    v.soc_target_kwh = 0.0;
    v.max_discharge_kw = 500.0;
    let s = base_scenario(8, vec![v], vec![charger(0, false)]);
    let r = run(&s, &MaxDischarge);
    assert_abs_diff_eq!(r.sessions[0].energy_exported_kwh, 0.0, epsilon = 1e-12);
    for rec in &r.slots {
        assert_abs_diff_eq!(rec.ev_discharge_kw, 0.0, epsilon = 1e-12);
    }
}

/// Reference assignment: EVERY car prefers a bidirectional port (not just
/// V2B-capable ones), ties broken by lowest charger id.
#[test]
fn assignment_prefers_bidirectional_for_every_car() {
    let mut v = vehicle(0, 0, 8);
    v.max_discharge_kw = 0.0; // not V2B-capable; still takes the bidi port
    let s = base_scenario(8, vec![v], vec![charger(0, false), charger(1, true)]);
    let r = run(
        &s,
        policy::by_name("policy-0").expect("registered").as_ref(),
    );
    assert_eq!(
        r.trace[0].charger_id, 1,
        "bidirectional port must be chosen first"
    );
}

/// POLICY_1's discharge stops at the SoC floor: the get_rate gate returns 0
/// below the floor and the engine clamps the final step at it.
#[test]
fn policy1_discharge_respects_floor() {
    let mut v = vehicle(0, 0, 48);
    v.soc_arrival_kwh = 35.0;
    v.soc_target_kwh = 10.0;
    v.min_soc_kwh = 30.0;
    let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![80.0; 48];
    for slot in 0..48 {
        s.tou_class[slot] = TouClass::Peak;
    }
    let r = run(
        &s,
        policy::by_name("policy-1").expect("registered").as_ref(),
    );
    for t in &r.trace {
        assert!(t.soc_kwh >= 30.0 - 1e-9, "below floor at slot {}", t.slot);
    }
    assert_abs_diff_eq!(r.sessions[0].soc_departure_kwh, 30.0, epsilon = 1e-9);
}

/// The reference EDF/LLF are DR-BLIND: the firm service level never enters
/// their budget, so with threshold headroom they charge straight through a
/// DR window and pay the penalty. This pins the faithful port; a mutant that
/// couples the budget to the FSL fails here.
#[test]
fn edf_llf_are_dr_blind() {
    for name in ["edf", "llf"] {
        let mut v = vehicle(0, 0, 24);
        v.soc_arrival_kwh = 5.0;
        v.soc_target_kwh = 55.0;
        let mut s = base_scenario(24, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![30.0; 24];
        s.manifest.heuristic_threshold_kw = Some(100.0);
        s.dr_events.push(dr_event(0, 12, 35.0)); // 5 kW below building+EV
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        assert!(
            r.bill.dr_penalty_usd > 0.0,
            "{name} must ignore the firm level (reference is DR-blind)"
        );
        assert!(r.sessions[0].target_met, "{name}: still meets the target");
    }
}

/// EDF sorts by deadline PRESSURE (need x charger rate / time), LLF by raw
/// time-left: with a binding budget they allocate differently in slot 0.
/// A: departs late with a big need (high pressure, long time). B: departs
/// soon with a tiny need (low pressure, short time).
#[test]
fn edf_pressure_vs_llf_time_ordering() {
    let build = || {
        let mut a = vehicle(0, 0, 40);
        a.soc_arrival_kwh = 10.0;
        a.soc_target_kwh = 40.0; // 30 kWh over 10 h: 3 kW metered
        let mut b = vehicle(1, 0, 12);
        b.soc_arrival_kwh = 38.0;
        b.soc_target_kwh = 40.0; // 2 kWh over 3 h: 0.667 kW metered
        let mut s = base_scenario(48, vec![a, b], vec![charger(0, true), charger(1, true)]);
        s.building_load_kw = vec![97.0; 48];
        s.manifest.heuristic_threshold_kw = Some(100.0); // 3 kW budget
        s
    };
    let power_at = |r: &openv2b::engine::Results, id: u32| {
        r.trace
            .iter()
            .find(|t| t.slot == 0 && t.vehicle_id == id)
            .map(|t| t.power_kw)
            .unwrap_or(0.0)
    };
    let edf = run(
        &build(),
        policy::by_name("edf").expect("registered").as_ref(),
    );
    let llf = run(
        &build(),
        policy::by_name("llf").expect("registered").as_ref(),
    );
    // EDF: A's pressure dominates; A takes the whole 3 kW budget, B starved.
    assert_abs_diff_eq!(power_at(&edf, 0), 3.0, epsilon = 1e-9);
    assert_abs_diff_eq!(power_at(&edf, 1), 0.0, epsilon = 1e-9);
    // LLF: B (shorter time_left) first at its metered 0.667 kW; A gets the
    // remainder via the reference's clip arithmetic.
    assert!(power_at(&llf, 1) > 0.5, "LLF must serve B first");
    assert!(power_at(&llf, 0) < 3.0, "LLF must leave A short in slot 0");
}
