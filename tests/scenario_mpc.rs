//! Scenario-MPC property tests: the SAA-specific machinery that the smoke
//! test in `tests/mpc.rs` does not pin.
//!
//! What makes these fixtures bind rather than pass for the wrong reason: the
//! connected session ARRIVES ON ITS TARGET, so nothing in the deterministic
//! part of the problem asks it to charge. Every kWh it takes is bought only
//! because a SAMPLED FUTURE wants it, which is exactly the coupling
//! (non-anticipativity + const-7 chaining) under test. Prices are cheap while
//! the car is connected and expensive afterwards, so banking is profitable in
//! a scenario that returns and strictly wasteful in one that does not.

#![cfg(feature = "solver-highs")]

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, vehicle};
use openv2b::engine::{run, Results};
use openv2b::milp::highs_backend::HighsBackend;
use openv2b::policy::scenario_mpc::{ScenarioMpc, ScenarioMpcConfig};
use openv2b::scenario::Scenario;

fn run_with(scenario: &Scenario, futures: Vec<Scenario>) -> Results {
    run(
        scenario,
        &ScenarioMpc::new(Box::new(HighsBackend), ScenarioMpcConfig::new(futures)),
    )
}

fn run_with_config(scenario: &Scenario, config: ScenarioMpcConfig) -> Results {
    run(scenario, &ScenarioMpc::new(Box::new(HighsBackend), config))
}

/// Vehicle 7 is connected for slots 0..8 and arrives exactly ON its target
/// (20 kWh): the live problem never needs a single kWh. Energy is cheap
/// (0.05) while it is connected and 12x more expensive (0.60) afterwards, so
/// buying now is worth 0.55 $/kWh to any scenario that still holds the energy
/// later, and a dead loss (0.05 $/kWh round trip, the degradation term making
/// the undo no better than free) to one that does not.
fn banking_fixture() -> Scenario {
    let mut v = vehicle(7, 0, 8);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 20.0;
    let mut s = base_scenario(96, vec![v], vec![charger(0, true)]);
    for slot in 0..96 {
        s.price_usd_per_kwh[slot] = if slot < 8 { 0.05 } else { 0.60 };
    }
    s
}

/// A sampled future in which the SAME identity returns later that day needing
/// 55 kWh: the const-7 chain makes its opening energy the connected session's
/// terminal energy minus the 5 kWh trip.
fn future_with_return() -> Scenario {
    let mut fv = vehicle(7, 40, 60);
    fv.soc_arrival_kwh = 30.0;
    fv.soc_target_kwh = 55.0;
    fv.depletion_kwh = 5.0;
    base_scenario(96, vec![fv], vec![charger(0, true)])
}

/// A sampled future in which nothing returns at all.
fn future_without_return() -> Scenario {
    base_scenario(96, Vec::new(), vec![charger(0, true)])
}

/// NON-ANTICIPATIVITY BINDS: the committed first slot is the SHARED optimum
/// over the sampled futures, not scenario 0's private optimum.
///
/// Scenario 0 is the future in which nothing returns; its private optimum at
/// slot 0 is to buy nothing. Scenario 1 wants a full 20 kW because the energy
/// survives (chained) into an expensive day. The `na_cp`/`na_cn` constraints
/// tie the two, so the committed rate is the joint optimum, and it must not
/// depend on which future was listed first.
///
/// MUTATION THIS KILLS: delete the non-anticipativity block in
/// `src/policy/scenario_mpc.rs` (the `for (k, sessions) in
/// scen_sessions.iter().enumerate().skip(1)` loop that adds `na_cp_*` /
/// `na_cn_*`). Scenario 0's first slot then floats free, the two-future
/// commitment collapses onto the `alone` reference, and both the
/// order-invariance and the strict-improvement assertions below fail.
#[test]
fn non_anticipativity_ties_the_committed_first_slot() {
    let s = banking_fixture();
    let both = run_with(&s, vec![future_without_return(), future_with_return()]);
    let swapped = run_with(&s, vec![future_with_return(), future_without_return()]);
    // The scenario-0-alone reference: exactly what the mutation degrades the
    // two-future commitment into.
    let alone = run_with(&s, vec![future_without_return()]);

    assert_abs_diff_eq!(
        both.slots[0].ev_charge_kw,
        swapped.slots[0].ev_charge_kw,
        epsilon = 1e-6
    );
    assert!(
        alone.slots[0].ev_charge_kw < 1e-6,
        "a lone no-return future must commit nothing at slot 0, got {} kW",
        alone.slots[0].ev_charge_kw
    );
    assert!(
        both.slots[0].ev_charge_kw > alone.slots[0].ev_charge_kw + 5.0,
        "the tied commitment must follow the returning future, not scenario 0: \
         {} kW with both futures vs {} kW with scenario 0 alone",
        both.slots[0].ev_charge_kw,
        alone.slots[0].ev_charge_kw
    );
}

/// CHAINED BANKING: a connected session whose identity returns in the sampled
/// futures banks strictly more than the same fixture with no future sessions.
///
/// This is the const-7 coupling seen from the outside: without the chain the
/// sampled return would open at its own CSV SoC and the connected session
/// would have no reason to overshoot its (already met) target.
#[test]
fn chained_future_makes_the_connected_session_bank() {
    let s = banking_fixture();
    let returning = run_with(&s, vec![future_with_return()]);
    let barren = run_with(&s, vec![future_without_return()]);

    let banked = &returning.sessions[0];
    let idle = &barren.sessions[0];
    assert!(
        idle.energy_drawn_kwh < 1e-6,
        "with no future sessions an on-target car must draw nothing, got {} kWh",
        idle.energy_drawn_kwh
    );
    assert!(
        banked.energy_drawn_kwh > idle.energy_drawn_kwh + 10.0,
        "the chained return must pull energy forward: {} kWh drawn vs {} kWh",
        banked.energy_drawn_kwh,
        idle.energy_drawn_kwh
    );
    assert!(
        banked.soc_departure_kwh > idle.soc_departure_kwh + 10.0,
        "banking must show up as departure SoC: {} kWh vs {} kWh",
        banked.soc_departure_kwh,
        idle.soc_departure_kwh
    );
    assert!(
        banked.banked_kwh > 0.0 && banked.target_met,
        "banking is surplus ABOVE a met target: banked {} kWh, target_met {}",
        banked.banked_kwh,
        banked.target_met
    );
    assert!(
        banked.soc_departure_kwh <= banked.soc_arrival_kwh + banked.energy_drawn_kwh + 1e-9,
        "lossless chaining cannot manufacture energy: {} kWh from {} + {}",
        banked.soc_departure_kwh,
        banked.soc_arrival_kwh,
        banked.energy_drawn_kwh
    );
}

/// A fixture whose CHEAP window is exactly where the sampled future
/// hallucinates a 300 kW building spike. Under the reference behavior
/// (`building_from_futures`) the planner believes the demand peak is already
/// lost in that window, so it charges there at full rate; told the realized
/// (flat) series instead, the same demand charge makes it spread the same
/// energy out. The realized peak separates the two.
fn forecast_fixture() -> (Scenario, Scenario) {
    let mut v = vehicle(0, 0, 48);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 50.0;
    let mut s = base_scenario(96, vec![v], vec![charger(0, true)]);
    s.manifest.demand_charge_usd_per_kw = 10.0;
    s.building_load_kw = vec![50.0; 96];
    for slot in 0..96 {
        s.price_usd_per_kwh[slot] = if slot < 24 { 0.20 } else { 0.05 };
    }
    let mut f = base_scenario(96, Vec::new(), vec![charger(0, true)]);
    f.building_load_kw = (0..96)
        .map(|slot| if slot < 24 { 50.0 } else { 300.0 })
        .collect();
    (s, f)
}

/// `building_from_futures`: sampling the building load changes the plan, and
/// BOTH settings stay inside every engine invariant.
#[test]
fn building_from_futures_changes_the_plan_and_breaks_nothing() {
    let (s, f) = forecast_fixture();
    let forecast = run_with_config(&s, ScenarioMpcConfig::new(vec![f.clone()]));
    let known = run_with_config(
        &s,
        ScenarioMpcConfig {
            building_from_futures: false,
            ..ScenarioMpcConfig::new(vec![f])
        },
    );

    assert!(
        forecast.bill.peak_net_kw > known.bill.peak_net_kw + 1.0,
        "a hallucinated 300 kW spike must change the realized profile: peak {} kW \
         (sampled load) vs {} kW (realized load)",
        forecast.bill.peak_net_kw,
        known.bill.peak_net_kw
    );
    for r in [&forecast, &known] {
        let session = &r.sessions[0];
        assert!(
            session.target_met,
            "the target is trivially feasible in both settings: missing {} kWh",
            session.missing_kwh
        );
        assert!(
            session.soc_departure_kwh >= 0.0 && session.soc_departure_kwh <= 60.0,
            "SoC must stay inside [floor, capacity], got {} kWh",
            session.soc_departure_kwh
        );
        assert_abs_diff_eq!(
            session.soc_departure_kwh,
            session.soc_arrival_kwh + session.energy_drawn_kwh - session.energy_exported_kwh,
            epsilon = 1e-6
        );
        for rec in &r.slots {
            assert!(
                rec.net_kw >= -1e-9,
                "no export at slot {}: net {} kW",
                rec.slot,
                rec.net_kw
            );
            assert!(
                rec.ev_charge_kw <= 20.0 + 1e-9 && rec.ev_discharge_kw <= 20.0 + 1e-9,
                "port limits hold at slot {}: +{} / -{} kW",
                rec.slot,
                rec.ev_charge_kw,
                rec.ev_discharge_kw
            );
        }
    }
}

/// Determinism with K > 1: the multi-scenario model is built and read back in
/// a fixed order, so two runs of the same K-future configuration must
/// serialize byte-identically (slots, sessions, and the bill).
#[test]
fn k_future_runs_are_deterministic() {
    let s = banking_fixture();
    let futures = || {
        vec![
            future_without_return(),
            future_with_return(),
            future_without_return(),
        ]
    };
    let a = run_with(&s, futures());
    let b = run_with(&s, futures());
    assert_eq!(
        serde_json::to_string(&a.slots).expect("serialize slots"),
        serde_json::to_string(&b.slots).expect("serialize slots"),
        "K=3 scenario-mpc must produce identical slot series"
    );
    assert_eq!(
        serde_json::to_string(&a.sessions).expect("serialize sessions"),
        serde_json::to_string(&b.sessions).expect("serialize sessions"),
        "K=3 scenario-mpc must produce identical session results"
    );
    assert_eq!(
        serde_json::to_string(&a.bill).expect("serialize bill"),
        serde_json::to_string(&b.bill).expect("serialize bill"),
        "K=3 scenario-mpc must produce an identical bill"
    );
}
