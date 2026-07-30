//! Negotiation pre-pass driver.
//!
//! Reads a scenario, runs the arrival-time contract negotiation, and writes
//! next to the input: `vehicles_negotiated.csv` (renegotiated departures and
//! targets; point the manifest's `vehicles_file` at it) and `contracts.csv`
//! (the full menus, utilities, and choices).
//!
//! Requires an in-process solver: build with `--features solver-highs`.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut scenario_dir = None;
    let mut seed: Option<u64> = None;
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--scenario" => scenario_dir = argv.next().map(PathBuf::from),
            "--seed" => seed = argv.next().and_then(|s| s.parse().ok()),
            _ => {
                eprintln!("usage: negotiate --scenario <dir> [--seed N]");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(dir) = scenario_dir else {
        eprintln!("usage: negotiate --scenario <dir> [--seed N]");
        return ExitCode::FAILURE;
    };
    run(&dir, seed)
}

#[cfg(not(feature = "solver-highs"))]
fn run(_dir: &std::path::Path, _seed: Option<u64>) -> ExitCode {
    eprintln!("negotiate requires building with --features solver-highs");
    ExitCode::FAILURE
}

#[cfg(feature = "solver-highs")]
fn run(dir: &std::path::Path, seed: Option<u64>) -> ExitCode {
    use openv2b::milp::highs_backend::HighsBackend;
    use openv2b::negotiation::{negotiate, NegotiationConfig};
    use openv2b::scenario::Scenario;
    use std::fmt::Write as _;

    let scenario = match Scenario::load(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load scenario: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut config = NegotiationConfig::default();
    if let Some(s) = seed {
        config.seed = s;
    }
    let (modified, records) = match negotiate(&scenario, &HighsBackend, &config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("negotiation failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut vcsv = String::from(
        "vehicle_id,arrival_slot,departure_slot,battery_kwh,soc_arrival_kwh,soc_target_kwh,\
         max_charge_kw,max_discharge_kw,min_soc_kwh,depletion_kwh\n",
    );
    for v in &modified.vehicles {
        writeln!(
            vcsv,
            "{},{},{},{},{},{},{},{},{},{}",
            v.vehicle_id,
            v.arrival_slot,
            v.departure_slot,
            v.battery_kwh,
            v.soc_arrival_kwh,
            v.soc_target_kwh,
            v.max_charge_kw,
            v.max_discharge_kw,
            v.min_soc_kwh,
            v.depletion_kwh
        )
        .expect("write to string");
    }
    let mut ccsv = String::from(
        "vehicle_id,arrival_slot,chosen_tier,chosen_is_reject,new_departure_slot,new_target_kwh,\
         tier,is_reject,delay_slots,target_reduction_kwh,price_usd,utility\n",
    );
    let mut accepted = 0usize;
    for r in &records {
        if !r.chosen_is_reject {
            accepted += 1;
        }
        for (o, u) in r.offers.iter().zip(&r.utilities) {
            writeln!(
                ccsv,
                "{},{},{},{},{},{},{},{},{},{},{:.4},{:.4}",
                r.vehicle_id,
                r.arrival_slot,
                r.chosen_tier,
                r.chosen_is_reject,
                r.new_departure_slot,
                r.new_target_kwh,
                o.tier,
                o.is_reject,
                o.delay_slots,
                o.target_reduction_kwh,
                o.price_usd,
                u
            )
            .expect("write to string");
        }
    }
    if std::fs::write(dir.join("vehicles_negotiated.csv"), vcsv).is_err()
        || std::fs::write(dir.join("contracts.csv"), ccsv).is_err()
    {
        eprintln!("failed to write outputs");
        return ExitCode::FAILURE;
    }
    println!(
        "negotiated {} sessions: {} accepted an offer, {} rejected; wrote vehicles_negotiated.csv + contracts.csv",
        records.len(),
        accepted,
        records.len() - accepted
    );
    ExitCode::SUCCESS
}
