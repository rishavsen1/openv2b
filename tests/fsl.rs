//! FSL commitment optimization tests.

#![cfg(feature = "solver-highs")]

mod common;

use common::{base_scenario, charger, dr_event, vehicle};
use openv2b::engine::run;
use openv2b::milp::highs_backend::HighsBackend;
use openv2b::policy::oracle::{solve_oracle, OracleConfig, OracleReplay};
use openv2b::scenario::Scenario;

/// One V2B donor, flat building load, one long DR window whose input firm
/// level is a weak commitment (barely below the baseline). The optimizer
/// should commit deeper, cover the window by discharging, and earn a larger
/// incentive.
fn fixture() -> Scenario {
    let mut v = vehicle(0, 0, 48);
    v.battery_kwh = 80.0;
    v.soc_arrival_kwh = 70.0;
    v.soc_target_kwh = 30.0;
    v.min_soc_kwh = 10.0;
    v.max_discharge_kw = 20.0;
    let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 48];
    let mut e = dr_event(16, 36, 48.0); // weak input commitment
    e.baseline_kw = 50.0;
    s.dr_events.push(e);
    s
}

fn optimized_plan(s: &Scenario) -> openv2b::policy::oracle::OraclePlan {
    let config = OracleConfig {
        optimize_fsl: true,
        ..OracleConfig::default()
    };
    solve_oracle(s, &HighsBackend, &config).expect("oracle solves fixture")
}

/// Rebuild the scenario with the committed firm levels and baselines so the
/// billing layer settles against the commitment.
fn committed_scenario(s: &Scenario, plan: &openv2b::policy::oracle::OraclePlan) -> Scenario {
    let mut out = s.clone();
    for (ei, e) in out.dr_events.iter_mut().enumerate() {
        e.fsl_kw = plan.committed_fsl_kw[ei];
        e.baseline_kw = plan.baseline_peak_kw[ei];
    }
    out
}

#[test]
fn committed_fsl_is_bounded_and_honored() {
    let s = fixture();
    let plan = optimized_plan(&s);
    assert!(
        plan.committed_fsl_kw[0] <= plan.baseline_peak_kw[0] + 1e-9,
        "commitment above the baseline peak"
    );
    // Simulate under the commitment: the window must be honored and paid.
    let committed = committed_scenario(&s, &plan);
    let r = run(&committed, &OracleReplay { plan });
    assert!(
        r.bill.dr_penalty_usd < 1e-6,
        "committed window violated (penalty {})",
        r.bill.dr_penalty_usd
    );
    assert!(
        r.bill.dr_incentive_usd > 0.0,
        "honored commitment earned nothing"
    );
    for sess in &r.sessions {
        assert!(sess.target_met, "target sacrificed for the commitment");
    }
}

#[test]
fn optimized_commitment_beats_the_weak_input_commitment() {
    let s = fixture();
    let plan = optimized_plan(&s);
    assert!(
        plan.committed_fsl_kw[0] < 48.0,
        "optimizer should commit deeper than the weak input F=48 (got {})",
        plan.committed_fsl_kw[0]
    );

    // Bill under the input commitment with the plain (non-FSL) oracle...
    let input_plan = solve_oracle(&s, &HighsBackend, &OracleConfig::default()).expect("solves");
    let input_bill = run(&s, &OracleReplay { plan: input_plan }).bill.total_usd;
    // ...versus under the optimized commitment.
    let committed = committed_scenario(&s, &plan);
    let committed_bill = run(&committed, &OracleReplay { plan }).bill.total_usd;
    assert!(
        committed_bill < input_bill,
        "optimized commitment ({committed_bill}) should beat the weak input ({input_bill})"
    );
}

/// Short-window overcommit guard: with a 4-covered-slot window the one-shot
/// incentive outweighs a few slots of soft penalty in the LP, but gated
/// billing pays nothing on a violated window. The post-adjustment must keep
/// the commitment honorable.
#[test]
fn short_window_commitment_stays_honorable() {
    let mut v = vehicle(0, 0, 48);
    v.max_discharge_kw = 0.0; // nothing can shave the window
    let mut s = base_scenario(48, vec![v], vec![charger(0, true)]);
    s.building_load_kw = vec![50.0; 48];
    let mut e = dr_event(20, 24, 50.0); // 4 covered slots
    e.baseline_kw = 50.0;
    e.incentive_usd_per_kw = 13.6;
    s.dr_events.push(e);
    let plan = optimized_plan(&s);
    let committed = committed_scenario(&s, &plan);
    let r = run(&committed, &OracleReplay { plan });
    assert!(
        r.bill.dr_penalty_usd < 1e-6,
        "post-adjustment failed: committed window is violated (penalty {})",
        r.bill.dr_penalty_usd
    );
}
