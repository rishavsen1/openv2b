//! Command-line entry point.
//!
//! ```text
//! openv2b --scenario <dir> --policy <name> [--out <dir>]
//! ```

use openv2b::{engine, output, policy, scenario::Scenario};
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    scenario: PathBuf,
    policy: String,
    out: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut scenario = None;
    let mut policy = None;
    let mut out = None;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--scenario" => {
                scenario = Some(PathBuf::from(
                    argv.next().ok_or("--scenario needs a value")?,
                ))
            }
            "--policy" => policy = Some(argv.next().ok_or("--policy needs a value")?),
            "--out" => out = Some(PathBuf::from(argv.next().ok_or("--out needs a value")?)),
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown argument '{other}'\n{}", usage())),
        }
    }
    Ok(Args {
        scenario: scenario.ok_or_else(|| format!("--scenario is required\n{}", usage()))?,
        policy: policy.ok_or_else(|| format!("--policy is required\n{}", usage()))?,
        out,
    })
}

fn usage() -> String {
    format!(
        "usage: openv2b --scenario <dir> --policy <name> [--out <dir>]\n  policies: {}",
        policy::POLICY_NAMES.join(", ")
    )
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let pol: Box<dyn policy::Policy> = match args.policy.as_str() {
        // MPC over the in-process HiGHS backend (build with
        // `--features solver-highs`). For other solvers (CPLEX, Xpress...)
        // construct `milp::cli::LpCliBackend` programmatically; see
        // docs/SOLVER_DESIGN.md.
        #[cfg(feature = "solver-highs")]
        "mpc" => Box::new(policy::mpc::Mpc::new(
            Box::new(openv2b::milp::highs_backend::HighsBackend),
            policy::mpc::MpcConfig::default(),
        )),
        #[cfg(not(feature = "solver-highs"))]
        "mpc" => {
            eprintln!("policy 'mpc' requires building with --features solver-highs");
            return ExitCode::FAILURE;
        }
        name => match policy::by_name(name) {
            Some(p) => p,
            None => {
                eprintln!("unknown policy '{}'\n{}", args.policy, usage());
                return ExitCode::FAILURE;
            }
        },
    };
    let scenario = match Scenario::load(&args.scenario) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load scenario: {e}");
            return ExitCode::FAILURE;
        }
    };

    let results = engine::run(&scenario, pol.as_ref());
    println!(
        "policy={} total=${:.2} (energy=${:.2} demand=${:.2} dr_penalty=${:.2} dr_incentive=${:.2}) peak={:.1} kW",
        results.policy,
        results.bill.total_usd,
        results.bill.energy_usd,
        results.bill.demand_usd,
        results.bill.dr_penalty_usd,
        results.bill.dr_incentive_usd,
        results.bill.peak_net_kw,
    );
    if let Some(out) = args.out {
        if let Err(e) = output::write_results(&out, &results) {
            eprintln!("failed to write results: {e}");
            return ExitCode::FAILURE;
        }
        println!("results written to {}", out.display());
    }
    ExitCode::SUCCESS
}
