//! Result writers: per-slot CSV, per-session CSV, and a JSON summary.

use crate::engine::Results;
use std::io::Write;
use std::path::Path;

/// Write `slots.csv`, `sessions.csv`, and `summary.json` into `dir`.
pub fn write_results(dir: &Path, results: &Results) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;

    let mut slot_writer = csv::Writer::from_path(dir.join("slots.csv"))?;
    for rec in &results.slots {
        slot_writer.serialize(rec)?;
    }
    slot_writer.flush()?;

    let mut session_writer = csv::Writer::from_path(dir.join("sessions.csv"))?;
    for rec in &results.sessions {
        session_writer.serialize(rec)?;
    }
    session_writer.flush()?;

    let mut trace_writer = csv::Writer::from_path(dir.join("trace.csv"))?;
    for rec in &results.trace {
        trace_writer.serialize(rec)?;
    }
    trace_writer.flush()?;

    let sum = |f: fn(&crate::engine::SessionResult) -> f64| -> f64 {
        results.sessions.iter().map(f).sum()
    };
    let summary = serde_json::json!({
        "policy": results.policy,
        "bill": results.bill,
        "sessions_total": results.sessions.len(),
        "sessions_target_met": results.sessions.iter().filter(|s| s.target_met).count(),
        "sessions_never_connected": results.sessions.iter().filter(|s| s.never_connected).count(),
        "energy_drawn_kwh": sum(|s| s.energy_drawn_kwh),
        "energy_exported_kwh": sum(|s| s.energy_exported_kwh),
        "missing_kwh": sum(|s| s.missing_kwh),
        "banked_kwh": sum(|s| s.banked_kwh),
        "chain_clamped_kwh": sum(|s| s.chain_clamped_kwh),
    });
    let mut f = std::fs::File::create(dir.join("summary.json"))?;
    writeln!(f, "{}", serde_json::to_string_pretty(&summary)?)?;
    Ok(())
}
