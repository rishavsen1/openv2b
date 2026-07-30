//! Regression tests for the R1 plan-review findings. Each test names the
//! finding it pins; all of these were demonstrated failing (or vacuous)
//! against the pre-review build.

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, dr_event, vehicle};
use openv2b::engine::run;
use openv2b::policy::{self, Policy, POLICY_NAMES};
use openv2b::state::{Observation, Setpoint};

/// R1-1 (CRITICAL): a DR window that abuts departure must not let V2B
/// discharge sacrifice the departure target. Geometry from the review probe:
/// arrive at target, building load above the firm level for the whole rest of
/// the session, so nothing below the target can ever be recovered. Banked
/// energy (charged above the target before the window) MAY be exported; the
/// SoC must simply never end below the target.
#[test]
fn dr_window_abutting_departure_cannot_sacrifice_target() {
    for name in ["edf-v2b", "llf-v2b"] {
        let mut v = vehicle(0, 0, 20);
        v.soc_arrival_kwh = 40.0;
        v.soc_target_kwh = 40.0;
        let mut s = base_scenario(24, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 24];
        s.dr_events.push(dr_event(4, 20, 40.0)); // covers slots 5..=20, departure at 20
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        assert!(
            r.sessions[0].target_met,
            "{name}: target sacrificed (SoC {} < 40)",
            r.sessions[0].soc_departure_kwh
        );
        assert!(
            r.sessions[0].soc_departure_kwh >= 40.0 - 1e-9,
            "{name}: departed below target"
        );
        // Anything exported must have been banked first, never taken from
        // the target reserve: exports <= eta_d * charged-above-arrival.
        let banked_in = r.sessions[0].energy_drawn_kwh; // arrival was at target
        assert!(
            r.sessions[0].energy_exported_kwh <= banked_in + 1e-9,
            "{name}: exported more than was banked"
        );
    }
}

/// R1-1 companion: surplus above the target IS discharged in the same geometry.
#[test]
fn surplus_above_target_still_discharges() {
    let mut v = vehicle(0, 0, 20);
    v.soc_arrival_kwh = 55.0;
    v.soc_target_kwh = 20.0;
    let mut s = base_scenario(24, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 24];
    s.dr_events.push(dr_event(4, 20, 40.0));
    let r = run(&s, policy::by_name("edf-v2b").expect("registered").as_ref());
    assert!(
        r.sessions[0].energy_exported_kwh > 0.0,
        "surplus must be used"
    );
    assert!(r.sessions[0].target_met, "and the target still met");
    assert!(
        r.sessions[0].soc_departure_kwh >= 20.0 - 1e-9,
        "never below target"
    );
}

/// R1-2 (CRITICAL): DR events outside the horizon are rejected at validation,
/// so no incentive can be paid for an unsimulated window.
#[test]
fn out_of_horizon_dr_event_rejected() {
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    let mut e = dr_event(1000, 2000, 5.0);
    e.baseline_kw = 100.0;
    s.dr_events.push(e);
    assert!(
        s.validate().is_err(),
        "out-of-horizon DR event must be rejected"
    );

    // Partially-out (end beyond horizon) is also rejected.
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.dr_events.push(dr_event(4, 8, 5.0)); // end_slot == horizon: slot 8 doesn't exist
    assert!(
        s.validate().is_err(),
        "end_slot == horizon must be rejected"
    );

    // An event ending at the last real slot is fine.
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.dr_events.push(dr_event(4, 7, 5.0));
    assert!(
        s.validate().is_ok(),
        "event ending at the final slot is valid"
    );
}

/// R1-3 (CRITICAL): overlapping DR events double-settle the same energy and
/// are rejected; back-to-back events are allowed.
#[test]
fn overlapping_dr_events_rejected_back_to_back_allowed() {
    let mut s = base_scenario(16, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.dr_events.push(dr_event(0, 8, 40.0));
    s.dr_events.push(dr_event(0, 8, 40.0));
    assert!(s.validate().is_err(), "identical events must be rejected");

    let mut s = base_scenario(16, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.dr_events.push(dr_event(0, 8, 40.0));
    s.dr_events.push(dr_event(8, 12, 40.0)); // (0,8] and (8,12] are disjoint
    assert!(s.validate().is_ok(), "back-to-back events are valid");
}

/// R1-8 (MAJOR): adversarial policies (NaN/inf/huge/out-of-range/duplicate
/// setpoints) can never corrupt the physics: charge and discharge columns
/// stay non-negative, caps hold, and energy is conserved.
struct Adversarial;

impl Policy for Adversarial {
    fn name(&self) -> &'static str {
        "adversarial-test"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut sp = Vec::new();
        for s in &obs.sessions {
            sp.push(Setpoint {
                session_index: s.index,
                power_kw: f64::NAN,
            });
            sp.push(Setpoint {
                session_index: s.index,
                power_kw: f64::INFINITY,
            });
            sp.push(Setpoint {
                session_index: s.index,
                power_kw: f64::NEG_INFINITY,
            });
            sp.push(Setpoint {
                session_index: s.index,
                power_kw: 1e15,
            });
            sp.push(Setpoint {
                session_index: s.index,
                power_kw: -1e15,
            });
        }
        sp.push(Setpoint {
            session_index: 9999,
            power_kw: 100.0,
        });
        sp
    }
}

#[test]
fn adversarial_setpoints_cannot_corrupt_physics() {
    let mut v = vehicle(0, 0, 40);
    v.soc_arrival_kwh = 30.0;
    let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 48];
    let r = run(&s, &Adversarial);
    for rec in &r.slots {
        assert!(
            rec.ev_charge_kw >= 0.0,
            "negative charge column at slot {}",
            rec.slot
        );
        assert!(
            rec.ev_discharge_kw >= 0.0,
            "negative discharge column at slot {}",
            rec.slot
        );
        assert!(rec.ev_charge_kw <= 20.0 + 1e-9, "charge above port cap");
        assert!(
            rec.ev_discharge_kw <= 20.0 + 1e-9,
            "discharge above port cap"
        );
        assert!(rec.net_kw >= -1e-9, "export at slot {}", rec.slot);
    }
    let sess = &r.sessions[0];
    assert_abs_diff_eq!(
        sess.soc_departure_kwh,
        sess.soc_arrival_kwh + sess.energy_drawn_kwh - sess.energy_exported_kwh,
        epsilon = 1e-9
    );
    assert!(sess.soc_departure_kwh >= -1e-9 && sess.soc_departure_kwh <= 60.0 + 1e-9);
}

/// R1-6 (MAJOR): the site cap is engine-enforced for EVERY policy, including
/// uncontrolled and adversarial ones. EV charging never pushes the site above
/// the cap (the building's own load is not curtailable, so the bound is
/// max(building, cap)).
#[test]
fn site_cap_is_engine_enforced() {
    let cap = 55.0;
    for name in POLICY_NAMES {
        let mut s = base_scenario(
            8,
            vec![vehicle(0, 0, 8), vehicle(1, 0, 8)],
            vec![charger(0, false), charger(1, false)],
        );
        s.building_load_kw = vec![50.0; 8];
        s.manifest.site_cap_kw = Some(cap);
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        for rec in &r.slots {
            assert!(
                rec.net_kw <= rec.building_kw.max(cap) + 1e-9,
                "policy {name}: net {} exceeds cap at slot {}",
                rec.net_kw,
                rec.slot
            );
        }
    }
    // Adversarial policy too.
    let mut s = base_scenario(8, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 8];
    s.manifest.site_cap_kw = Some(cap);
    let r = run(&s, &Adversarial);
    for rec in &r.slots {
        assert!(
            rec.net_kw <= rec.building_kw.max(cap) + 1e-9,
            "adversarial exceeds cap"
        );
    }
}

/// R1-7 (MAJOR): capability-aware assignment: a V2B donor arriving after a
/// non-V2B vehicle still ends up on the bidirectional port and can discharge.
#[test]
fn v2b_donor_gets_bidirectional_port() {
    let mut v0 = vehicle(0, 0, 8);
    v0.max_discharge_kw = 0.0; // arrives first, cannot V2B
    let mut v1 = vehicle(1, 0, 8);
    v1.soc_arrival_kwh = 55.0;
    v1.soc_target_kwh = 10.0; // arrives second, big surplus
    let mut s = base_scenario(8, vec![v0, v1], vec![charger(0, false), charger(1, true)]);
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
        "donor stranded on a unidirectional port"
    );
}

/// Referee catch (lossy month run): the discharge budget is battery-side
/// energy but setpoints are building-side power; without applying eta_d the
/// battery dips below the reserved level by the conversion loss. The reserve
/// must hold under asymmetric efficiencies.
#[test]
fn lossy_discharge_never_dips_below_reserve() {
    for name in ["edf-v2b", "llf-v2b"] {
        let mut v = vehicle(0, 0, 40);
        v.soc_arrival_kwh = 55.0;
        v.soc_target_kwh = 40.0;
        let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
        s.manifest.charge_efficiency = 0.92;
        s.manifest.discharge_efficiency = 0.94;
        s.building_load_kw = vec![60.0; 48];
        s.dr_events.push(dr_event(2, 40, 40.0)); // deep window, big shortfall
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        for t in &r.trace {
            assert!(
                t.soc_kwh >= 40.0 - 1e-9,
                "{name}: slot {} SoC {} dipped below the 40 kWh reserve",
                t.slot,
                t.soc_kwh
            );
        }
        assert!(r.sessions[0].target_met, "{name}: target must hold");
    }
}

/// R1-16 (MAJOR) P21: site energy balance ties slots.csv to sessions.csv:
/// sum(net * dt) = building energy + total drawn - total exported.
#[test]
fn site_energy_balance() {
    for name in POLICY_NAMES {
        let mut v1 = vehicle(0, 0, 40);
        v1.soc_arrival_kwh = 50.0;
        v1.soc_target_kwh = 30.0;
        let mut v2 = vehicle(1, 4, 30);
        v2.soc_arrival_kwh = 5.0;
        v2.soc_target_kwh = 45.0;
        let mut s = base_scenario(48, vec![v1, v2], vec![charger(0, true), charger(1, false)]);
        s.dr_events.push(dr_event(12, 20, 40.0));
        let r = run(&s, policy::by_name(name).expect("registered").as_ref());
        let net_kwh: f64 = r.slots.iter().map(|rec| rec.net_kw * 0.25).sum();
        let building_kwh: f64 = r.slots.iter().map(|rec| rec.building_kw * 0.25).sum();
        let drawn: f64 = r.sessions.iter().map(|x| x.energy_drawn_kwh).sum();
        let exported: f64 = r.sessions.iter().map(|x| x.energy_exported_kwh).sum();
        assert_abs_diff_eq!(net_kwh, building_kwh + drawn - exported, epsilon = 1e-9);
    }
}

/// R1-19 (MAJOR): itemized bill identity with both demand components, and the
/// peak-TOU peak never exceeds the all-slots peak.
#[test]
fn itemized_bill_identity_and_peak_ordering() {
    use openv2b::scenario::TouClass;
    let mut s = base_scenario(16, vec![vehicle(0, 0, 8)], vec![charger(0, true)]);
    for slot in 4..10 {
        s.tou_class[slot] = TouClass::Peak;
    }
    s.manifest.demand_charge_usd_per_kw = 3.0;
    s.manifest.demand_charge_peak_usd_per_kw = 10.0;
    s.dr_events.push(dr_event(2, 6, 40.0));
    let r = run(&s, policy::by_name("edf").expect("registered").as_ref());
    let b = &r.bill;
    assert_abs_diff_eq!(
        b.demand_usd,
        b.demand_facilities_usd + b.demand_peak_tou_usd,
        epsilon = 1e-9
    );
    assert_abs_diff_eq!(
        b.total_usd,
        b.energy_usd + b.demand_usd + b.dr_penalty_usd - b.dr_incentive_usd,
        epsilon = 1e-9
    );
    assert!(
        b.peak_net_peak_tou_kw <= b.peak_net_kw + 1e-9,
        "peak-TOU peak > overall peak"
    );
}

/// R1-21 (MINOR): permutation invariance: shuffling vehicle CSV rows does not
/// change any session outcome (results are keyed by identity, not row order).
#[test]
fn vehicle_row_permutation_invariance() {
    let mk = |order: &[usize]| {
        let protos = [
            (0u32, 0usize, 10usize, 20.0),
            (1, 2, 30, 25.0),
            (2, 4, 20, 30.0),
        ];
        let vehicles = order
            .iter()
            .map(|&k| {
                let (id, arr, dep, soc) = protos[k];
                let mut v = vehicle(id, arr, dep);
                v.soc_arrival_kwh = soc;
                v
            })
            .collect();
        base_scenario(48, vehicles, vec![charger(0, true), charger(1, false)])
    };
    for name in POLICY_NAMES {
        let pol = policy::by_name(name).expect("registered");
        let a = run(&mk(&[0, 1, 2]), pol.as_ref());
        let b = run(&mk(&[2, 0, 1]), pol.as_ref());
        assert_eq!(
            serde_json::to_string(&a.sessions).expect("serialize"),
            serde_json::to_string(&b.sessions).expect("serialize"),
            "policy {name}: row order changed outcomes"
        );
    }
}

/// R1-24: the trace output allows checking charger exclusivity directly:
/// no charger serves two sessions in the same slot.
#[test]
fn trace_shows_charger_exclusivity() {
    let mut s = base_scenario(
        48,
        vec![vehicle(0, 0, 40), vehicle(1, 2, 30), vehicle(2, 4, 20)],
        vec![charger(0, true), charger(1, false)],
    );
    s.dr_events.push(dr_event(12, 20, 40.0));
    let r = run(&s, policy::by_name("llf-v2b").expect("registered").as_ref());
    let mut seen = std::collections::HashSet::new();
    for t in &r.trace {
        assert!(
            seen.insert((t.slot, t.charger_id)),
            "charger {} double-booked at slot {}",
            t.charger_id,
            t.slot
        );
    }
    // And the trace reconciles exactly with the slot aggregates.
    for rec in &r.slots {
        let charge: f64 = r
            .trace
            .iter()
            .filter(|t| t.slot == rec.slot && t.power_kw > 0.0)
            .map(|t| t.power_kw)
            .sum();
        let discharge: f64 = r
            .trace
            .iter()
            .filter(|t| t.slot == rec.slot && t.power_kw < 0.0)
            .map(|t| -t.power_kw)
            .sum();
        assert_abs_diff_eq!(charge, rec.ev_charge_kw, epsilon = 1e-9);
        assert_abs_diff_eq!(discharge, rec.ev_discharge_kw, epsilon = 1e-9);
    }
}
