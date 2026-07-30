//! Firm-service-level commitment planner.
//!
//! Reads a scenario, solves the full-horizon oracle with the firm level of
//! each DR event as a decision variable (bounded by the counterfactual no-DR
//! baseline peak), and writes `dr_events_committed.csv` next to the input:
//! the same schema with `fsl_kw` replaced by the optimized commitment and
//! `baseline_kw` by the computed counterfactual peak. Point the scenario's
//! `dr_events_file` at it to simulate under the commitment.
//!
//! Requires an in-process solver: build with `--features solver-highs`.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1);
    let usage = "usage: plan_fsl --scenario <dir>";
    let scenario_dir = match (argv.next().as_deref(), argv.next()) {
        (Some("--scenario"), Some(dir)) => PathBuf::from(dir),
        _ => {
            eprintln!("{usage}");
            return ExitCode::FAILURE;
        }
    };
    run(&scenario_dir)
}

#[cfg(not(feature = "solver-highs"))]
fn run(_scenario_dir: &std::path::Path) -> ExitCode {
    eprintln!("plan_fsl requires building with --features solver-highs");
    ExitCode::FAILURE
}

#[cfg(feature = "solver-highs")]
fn run(scenario_dir: &std::path::Path) -> ExitCode {
    use openv2b::milp::highs_backend::HighsBackend;
    use openv2b::policy::oracle::{solve_oracle, OracleConfig};
    use openv2b::scenario::Scenario;
    use std::fmt::Write as _;

    let scenario = match Scenario::load(scenario_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load scenario: {e}");
            return ExitCode::FAILURE;
        }
    };
    if scenario.dr_events.is_empty() {
        eprintln!("scenario has no DR events; nothing to commit");
        return ExitCode::FAILURE;
    }
    let config = OracleConfig {
        optimize_fsl: true,
        ..OracleConfig::default()
    };
    let plan = match solve_oracle(&scenario, &HighsBackend, &config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oracle solve failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut out = String::from(
        "start_slot,end_slot,fsl_kw,penalty_usd_per_kwh,incentive_usd_per_kw,baseline_kw\n",
    );
    for (ei, e) in scenario.dr_events.iter().enumerate() {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            e.start_slot,
            e.end_slot,
            plan.committed_fsl_kw[ei],
            e.penalty_usd_per_kwh,
            e.incentive_usd_per_kw,
            plan.baseline_peak_kw[ei],
        )
        .expect("write to string");
        println!(
            "event ({}, {}]: input F = {:.2} kW -> committed F = {:.2} kW (baseline {:.2} kW)",
            e.start_slot,
            e.end_slot,
            e.fsl_kw,
            plan.committed_fsl_kw[ei],
            plan.baseline_peak_kw[ei]
        );
    }
    let path = scenario_dir.join("dr_events_committed.csv");
    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("failed to write {}: {e}", path.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", path.display());
    ExitCode::SUCCESS
}
