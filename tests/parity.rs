//! MPC-vs-oracle parity suites and the drift canary.
//!
//! The inherited lesson these tests encode: a verified zero gap on the
//! DEFICIT regime (arrive low, need high, no V2B) certifies nothing about
//! the SURPLUS regime (arrive high, need low, V2B-heavy) with staggered
//! departures; receding-horizon information-loss bugs hide only there. Both
//! regimes are pinned, and a canary asserts the planned peak never rises
//! between re-solves under perfect foresight.
//!
//! Parity precondition: the fixtures give MPC the same information set as
//! the oracle (all sessions connected from slot 0, single session per
//! vehicle, no charger contention), so the receding-horizon re-solves are
//! shrink-horizon restatements of one LP and the realized bills must match.

#![cfg(feature = "solver-highs")]

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, dr_event, vehicle};
use openv2b::engine::run;
use openv2b::milp::highs_backend::HighsBackend;
use openv2b::policy::mpc::{Mpc, MpcConfig};
use openv2b::policy::oracle::{solve_oracle, OracleConfig, OracleReplay};
use openv2b::scenario::Scenario;

fn mpc() -> Mpc {
    Mpc::new(Box::new(HighsBackend), MpcConfig::default())
}

fn oracle_replay(s: &Scenario) -> OracleReplay {
    let plan =
        solve_oracle(s, &HighsBackend, &OracleConfig::default()).expect("oracle solves fixture");
    OracleReplay { plan }
}

/// Deficit regime: three vehicles arrive at slot 0 nearly empty with high
/// targets and NO discharge capability; staggered departures; TOU prices,
/// priced demand, one DR window.
fn deficit_fixture() -> Scenario {
    let mk = |id: u32, dep: usize, target: f64| {
        let mut v = vehicle(id, 0, dep);
        v.soc_arrival_kwh = 8.0;
        v.soc_target_kwh = target;
        v.max_discharge_kw = 0.0;
        v
    };
    let mut s = base_scenario(
        48,
        vec![mk(0, 16, 40.0), mk(1, 28, 50.0), mk(2, 44, 55.0)],
        vec![charger(0, true), charger(1, true), charger(2, true)],
    );
    s.building_load_kw = vec![30.0; 48];
    // Expensive first, cheap later: immediate charging (uncontrolled) is the
    // WRONG strategy here, so the oracle has something to optimize; the
    // earliest departure still forces some expensive charging.
    for slot in 0..48 {
        s.price_usd_per_kwh[slot] = if slot < 16 { 0.35 } else { 0.12 };
        if (24..40).contains(&slot) {
            s.tou_class[slot] = openv2b::scenario::TouClass::Peak;
        }
    }
    s.manifest.demand_charge_peak_usd_per_kw = 11.67;
    s.dr_events.push(dr_event(20, 32, 45.0));
    s
}

/// Surplus regime: three V2B vehicles arrive at slot 0 far above their
/// targets, staggered departures, building load above the firm level for a
/// long mid-horizon window. This is where departed-bank/information-loss
/// bugs live.
fn surplus_fixture() -> Scenario {
    let mk = |id: u32, dep: usize| {
        let mut v = vehicle(id, 0, dep);
        v.battery_kwh = 80.0;
        v.soc_arrival_kwh = 70.0;
        v.soc_target_kwh = 25.0;
        v.min_soc_kwh = 10.0;
        v.max_discharge_kw = 15.0;
        v.max_charge_kw = 15.0;
        v
    };
    let mut s = base_scenario(
        48,
        vec![mk(0, 18), mk(1, 30), mk(2, 46)],
        vec![charger(0, true), charger(1, true), charger(2, true)],
    );
    s.building_load_kw = vec![55.0; 48];
    for slot in 0..48 {
        s.price_usd_per_kwh[slot] = if slot < 12 { 0.10 } else { 0.30 };
        if (16..40).contains(&slot) {
            s.tou_class[slot] = openv2b::scenario::TouClass::Peak;
        }
    }
    s.manifest.demand_charge_peak_usd_per_kw = 11.67;
    s.dr_events.push(dr_event(8, 40, 45.0)); // long window, 10 kW shortfall
    s
}

/// The battery-wear objective term (0.05 $/kWh) is unbilled, so realized
/// bills may legitimately differ by up to that term; the comparison slack
/// accounts for it explicitly rather than papering over it.
fn deg_slack(r: &openv2b::engine::Results) -> f64 {
    0.05 * r
        .sessions
        .iter()
        .map(|s| s.energy_exported_kwh)
        .sum::<f64>()
        + 1e-4
}

#[test]
fn deficit_regime_mpc_matches_oracle() {
    let s = deficit_fixture();
    let m = run(&s, &mpc());
    let o = run(&s, &oracle_replay(&s));
    for r in [&m, &o] {
        for sess in &r.sessions {
            assert!(sess.target_met, "{}: target missed", r.policy);
        }
    }
    assert_abs_diff_eq!(
        m.bill.total_usd,
        o.bill.total_usd,
        epsilon = deg_slack(&m).max(deg_slack(&o))
    );
}

#[test]
fn surplus_regime_mpc_matches_oracle() {
    let s = surplus_fixture();
    let m = run(&s, &mpc());
    let o = run(&s, &oracle_replay(&s));
    for r in [&m, &o] {
        for sess in &r.sessions {
            assert!(sess.target_met, "{}: target missed", r.policy);
        }
    }
    assert_abs_diff_eq!(
        m.bill.total_usd,
        o.bill.total_usd,
        epsilon = deg_slack(&m).max(deg_slack(&o))
    );
    // The whole point of the surplus fixture: V2B actually fires.
    let exported: f64 = m.sessions.iter().map(|x| x.energy_exported_kwh).sum();
    assert!(
        exported > 10.0,
        "surplus fixture must be V2B-heavy (exported {exported})"
    );
}

/// Drift canary: under perfect foresight (all sessions known at slot 0),
/// the planned peak must never jump UPWARD between successive re-solves.
/// An upward jump means the controller lost information it had before:
/// the failure class behind the departed-bank bug.
#[test]
fn planned_peak_never_rises_under_perfect_foresight() {
    for fixture in [deficit_fixture(), surplus_fixture()] {
        let policy = mpc();
        let _ = run(&fixture, &policy);
        let peaks = policy.planned_peaks.borrow();
        assert!(!peaks.is_empty(), "canary needs recorded solves");
        for pair in peaks.windows(2) {
            assert!(
                pair[1] <= pair[0] + 1e-6,
                "planned peak rose between re-solves: {} -> {} (information loss)",
                pair[0],
                pair[1]
            );
        }
    }
}

/// Oracle sanity on both regimes: it beats uncontrolled by a wide margin
/// (uncontrolled has no banking/V2B, so the unbilled-degradation caveat
/// cannot flip this ordering).
#[test]
fn oracle_beats_uncontrolled() {
    for s in [deficit_fixture(), surplus_fixture()] {
        let o = run(&s, &oracle_replay(&s));
        let u = run(
            &s,
            openv2b::policy::by_name("uncontrolled")
                .expect("registered")
                .as_ref(),
        );
        assert!(
            o.bill.total_usd < u.bill.total_usd,
            "oracle {} !< uncontrolled {}",
            o.bill.total_usd,
            u.bill.total_usd
        );
    }
}

/// The oracle's persistence coupling: with two chained sessions and a DR
/// window in the second, the oracle banks in session 1 (cheap) to discharge
/// in session 2, which an independent-session solve cannot do.
#[test]
fn oracle_banks_across_the_persistence_chain() {
    let mk = |arr: usize, dep: usize, depl: f64| {
        let mut v = vehicle(0, arr, dep);
        v.battery_kwh = 60.0;
        v.soc_arrival_kwh = 20.0;
        v.soc_target_kwh = 25.0;
        v.max_discharge_kw = 20.0;
        v.depletion_kwh = depl;
        v
    };
    let mut s = base_scenario(
        96,
        vec![mk(0, 24, 0.0), mk(40, 90, 5.0)],
        vec![charger(0, true)],
    );
    s.building_load_kw = vec![50.0; 96];
    for slot in 0..96 {
        s.price_usd_per_kwh[slot] = if slot < 24 { 0.08 } else { 0.30 };
    }
    // 8 kW shortfall over 28 covered slots = 56 kWh: coverable from a full
    // 60 kWh battery at the window start, so zero overflow is attainable
    // (a longer window would exceed the battery and force a penalty).
    s.dr_events.push(dr_event(48, 76, 42.0));
    let plan = solve_oracle(&s, &HighsBackend, &OracleConfig::default()).expect("solves");
    let r = run(&s, &OracleReplay { plan });
    let session1 = &r.sessions[0];
    assert!(
        session1.banked_kwh > 5.0,
        "oracle should bank in the cheap first session (banked {})",
        session1.banked_kwh
    );
    assert!(
        r.bill.dr_penalty_usd < 1e-6,
        "banked energy should cover the window (penalty {})",
        r.bill.dr_penalty_usd
    );
}
