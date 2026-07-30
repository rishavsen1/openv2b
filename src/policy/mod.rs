//! The [`Policy`] trait and built-in scheduling heuristics.

mod heuristics;
pub mod mpc;
pub mod oracle;

pub use heuristics::{EarliestDeadlineFirst, LeastLaxityFirst, Uncontrolled};

use crate::state::{Observation, Setpoint};

/// Never charges or discharges anything. The building-only baseline for
/// EV-vs-building cost attribution.
pub struct Idle;

impl Policy for Idle {
    fn name(&self) -> &'static str {
        "idle"
    }
    fn decide(&self, _obs: &Observation) -> Vec<Setpoint> {
        Vec::new()
    }
}

/// A charging policy: maps the observable state of one slot to a power
/// setpoint per connected session. Implementations must be deterministic
/// and side-effect free.
pub trait Policy {
    fn name(&self) -> &'static str;
    fn decide(&self, obs: &Observation) -> Vec<Setpoint>;
}

/// Look up a built-in policy by name (used by the CLI).
pub fn by_name(name: &str) -> Option<Box<dyn Policy>> {
    match name {
        "idle" => Some(Box::new(Idle)),
        "uncontrolled" => Some(Box::new(Uncontrolled)),
        "edf" => Some(Box::new(EarliestDeadlineFirst { v2b: false })),
        "edf-v2b" => Some(Box::new(EarliestDeadlineFirst { v2b: true })),
        "llf" => Some(Box::new(LeastLaxityFirst { v2b: false })),
        "llf-v2b" => Some(Box::new(LeastLaxityFirst { v2b: true })),
        _ => None,
    }
}

/// Names accepted by [`by_name`], for CLI help output.
pub const POLICY_NAMES: &[&str] = &["idle", "uncontrolled", "edf", "edf-v2b", "llf", "llf-v2b"];
