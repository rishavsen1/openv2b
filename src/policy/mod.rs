//! The [`Policy`] trait and built-in scheduling heuristics.

mod heuristics;
pub mod mpc;
pub mod oracle;
pub mod scenario_mpc;

pub use heuristics::{Idle, Policy0, Policy1, Policy2, ThresholdScheduler, Uncontrolled};

use crate::state::{Observation, Setpoint};

/// A charging policy: maps the observable state of one slot to a power
/// setpoint per connected session. Implementations must be deterministic
/// given the run; they may keep private per-episode state (e.g. the EDF/LLF
/// threshold ratchet, mirroring the reference's instance attributes) but
/// must not touch the environment.
pub trait Policy {
    fn name(&self) -> &'static str;
    fn decide(&self, obs: &Observation) -> Vec<Setpoint>;
}

/// Look up a built-in policy by name (used by the CLI).
pub fn by_name(name: &str) -> Option<Box<dyn Policy>> {
    match name {
        "idle" => Some(Box::new(Idle)),
        "uncontrolled" => Some(Box::new(Uncontrolled)),
        "policy-0" => Some(Box::new(Policy0)),
        "policy-1" => Some(Box::new(Policy1)),
        "policy-2" => Some(Box::new(Policy2)),
        "edf" => Some(Box::new(ThresholdScheduler::edf())),
        "llf" => Some(Box::new(ThresholdScheduler::llf())),
        _ => None,
    }
}

/// Names accepted by [`by_name`], for CLI help output. `edf`/`llf` and
/// `policy-0/1/2` are faithful OPTIMUS ports; POLICY_3 is deliberately
/// omitted (non-functional in the reference: its discharge leg calls a
/// method that does not exist).
pub const POLICY_NAMES: &[&str] = &[
    "idle",
    "uncontrolled",
    "policy-0",
    "policy-1",
    "policy-2",
    "edf",
    "llf",
];
