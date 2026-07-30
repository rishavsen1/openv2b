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

/// M10: the discharge reserve must respect the SoC floor, not just the
/// target. Two donors with floor 45 > target 10 hold exactly 1 kWh of true
/// surplus each; an 8 kW shortfall needs BOTH. If the reserve ignored the
/// floor, the first-visited donor would claim the whole shortfall, get
/// engine-clamped to its 1 kWh, and the slot would deliver only half the
/// available relief.
#[test]
fn discharge_reserve_respects_floor_across_donors() {
    let mk = |id: u32| {
        let mut v = vehicle(id, 0, 12);
        v.min_soc_kwh = 45.0;
        v.soc_target_kwh = 10.0;
        v.soc_arrival_kwh = 46.0;
        v
    };
    let mut s = base_scenario(
        16,
        vec![mk(0), mk(1)],
        vec![charger(0, true), charger(1, true)],
    );
    s.building_load_kw = vec![48.0; 16];
    s.dr_events.push(dr_event(0, 8, 40.0)); // 8 kW shortfall, covers slots 1..=8
    let r = run(&s, policy::by_name("edf-v2b").expect("registered").as_ref());
    assert_abs_diff_eq!(r.slots[1].ev_discharge_kw, 8.0, epsilon = 1e-9);
}

/// M12: capability-aware assignment must work regardless of charger index
/// order. Bidirectional port FIRST: lowest-free-index would hand it to the
/// non-V2B early arriver and strand the donor.
#[test]
fn donor_gets_bidi_port_even_when_bidi_port_is_index_zero() {
    let mut v0 = vehicle(0, 0, 8);
    v0.max_discharge_kw = 0.0; // arrives first (lower id), cannot V2B
    let mut v1 = vehicle(1, 0, 8);
    v1.soc_arrival_kwh = 55.0;
    v1.soc_target_kwh = 10.0;
    let mut s = base_scenario(8, vec![v0, v1], vec![charger(0, true), charger(1, false)]);
    s.building_load_kw = vec![50.0; 8];
    s.dr_events.push(dr_event(0, 7, 40.0));
    let r = run(&s, policy::by_name("edf-v2b").expect("registered").as_ref());
    let donor = r
        .sessions
        .iter()
        .find(|x| x.vehicle_id == 1)
        .expect("donor session");
    assert!(
        donor.energy_exported_kwh > 0.0,
        "donor stranded on the unidirectional port"
    );
}

/// M13: charge-only priority policies must curtail charging to the firm
/// level inside a DR window (this is the FSL headroom term, previously never
/// exercised because DR-test vehicles arrived already at target).
#[test]
fn priority_policies_curtail_charging_to_firm_level() {
    let build = || {
        let mut v = vehicle(0, 0, 40);
        v.battery_kwh = 80.0;
        v.soc_arrival_kwh = 5.0;
        v.soc_target_kwh = 65.0;
        let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![30.0; 48];
        s.dr_events.push(dr_event(0, 12, 40.0)); // 10 kW of in-window headroom
        s
    };
    for name in ["edf", "llf", "edf-v2b", "llf-v2b"] {
        let r = run(
            &build(),
            policy::by_name(name).expect("registered").as_ref(),
        );
        for rec in r.slots.iter().filter(|rec| rec.slot > 0 && rec.slot <= 12) {
            assert!(
                rec.net_kw <= 40.0 + 1e-9,
                "{name}: charged past the firm level at slot {} ({} kW)",
                rec.slot,
                rec.net_kw
            );
        }
        assert!(
            r.sessions[0].target_met,
            "{name}: target still met after the window"
        );
    }
    // Positive control: uncontrolled ignores the firm level.
    let r = run(
        &build(),
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    assert!(
        r.slots[1].net_kw > 40.0 + 1e-9,
        "uncontrolled should exceed the firm level (got {})",
        r.slots[1].net_kw
    );
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

/// Audit gap (a)(6): LLF must order differently from EDF. Vehicle A departs
/// LATER but has LOWER laxity (38 kWh of need at only 4 kW: laxity = 40 - 38
/// = 2 slots); B departs sooner with a tiny need (laxity = 12 - 0.2 = 11.8).
/// Under a 5 kW shared headroom, EDF serves B first (earlier departure), LLF
/// serves A first (lower laxity): the first-slot allocations differ.
#[test]
fn llf_orders_by_laxity_not_departure() {
    let build = || {
        let mut a = vehicle(0, 0, 40); // departs later, urgent
        a.soc_arrival_kwh = 5.0;
        a.soc_target_kwh = 43.0;
        a.max_charge_kw = 4.0;
        let mut b = vehicle(1, 0, 12); // departs sooner, relaxed
        b.soc_arrival_kwh = 39.0;
        b.soc_target_kwh = 40.0;
        let mut s = base_scenario(48, vec![a, b], vec![charger(0, true), charger(1, true)]);
        s.building_load_kw = vec![50.0; 48];
        s.manifest.site_cap_kw = Some(55.0); // 5 kW headroom for the fleet
        s
    };
    let edf = run(
        &build(),
        policy::by_name("edf").expect("registered").as_ref(),
    );
    let llf = run(
        &build(),
        policy::by_name("llf").expect("registered").as_ref(),
    );
    let first_a = |r: &openv2b::engine::Results| {
        r.trace
            .iter()
            .find(|t| t.slot == 0 && t.vehicle_id == 0)
            .expect("vehicle 0 traced")
            .power_kw
    };
    // EDF: B (dep 12) takes 4 kW of the 5 kW headroom, A gets the remaining 1.
    // LLF: A (laxity 2) takes its full 4 kW, B gets the remaining 1.
    assert_abs_diff_eq!(first_a(&edf), 1.0, epsilon = 1e-9);
    assert_abs_diff_eq!(first_a(&llf), 4.0, epsilon = 1e-9);
}
