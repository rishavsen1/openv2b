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
    let scenario = match Scenario::load(&args.scenario) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load scenario: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The CPLEX CLI backend is dependency-free and available in every build;
    // the binary path comes from OPENV2B_CPLEX_BIN.
    let cplex_backend = || -> Box<dyn openv2b::milp::MilpBackend> {
        let bin = std::env::var("OPENV2B_CPLEX_BIN")
            .unwrap_or_else(|_| "/home/rishav/ibm/cplex/bin/x86-64_linux/cplex".into());
        Box::new(openv2b::milp::cli::LpCliBackend::cplex(
            bin,
            std::env::temp_dir().join("openv2b_cplex_cli"),
        ))
    };
    let oracle_policy =
        |backend: &dyn openv2b::milp::MilpBackend| -> Option<Box<dyn policy::Policy>> {
            match policy::oracle::solve_oracle(
                &scenario,
                backend,
                &policy::oracle::OracleConfig::default(),
            ) {
                Ok(plan) => Some(Box::new(policy::oracle::OracleReplay { plan })),
                Err(e) => {
                    eprintln!("oracle solve failed: {e}");
                    None
                }
            }
        };

    let pol: Box<dyn policy::Policy> = match args.policy.as_str() {
        // MPC over the in-process HiGHS backend (build with
        // `--features solver-highs`); `mpc-cplex` / `oracle-cplex` drive the
        // CPLEX CLI through the LP-file backend and work in every build.
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
        "mpc-cplex" => Box::new(policy::mpc::Mpc::new(
            cplex_backend(),
            policy::mpc::MpcConfig::default(),
        )),
        #[cfg(feature = "solver-highs")]
        "oracle" => match oracle_policy(&openv2b::milp::highs_backend::HighsBackend) {
            Some(p) => p,
            None => return ExitCode::FAILURE,
        },
        #[cfg(not(feature = "solver-highs"))]
        "oracle" => {
            eprintln!("policy 'oracle' requires building with --features solver-highs");
            return ExitCode::FAILURE;
        }
        "oracle-cplex" => match oracle_policy(cplex_backend().as_ref()) {
            Some(p) => p,
            None => return ExitCode::FAILURE,
        },
        name => match policy::by_name(name) {
            Some(p) => p,
            None => {
                eprintln!("unknown policy '{}'\n{}", args.policy, usage());
                return ExitCode::FAILURE;
            }
        },
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
