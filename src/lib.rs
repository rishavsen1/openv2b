//! openv2b: a lightweight discrete-event simulator for EV vehicle-to-building
//! (V2B) charging and scheduling research.
//!
//! The crate is organized as:
//! - [`scenario`]: input model (vehicles, chargers, building load, prices, DR events)
//! - [`state`]: mutable simulation state
//! - [`policy`]: the [`policy::Policy`] trait and built-in heuristics
//! - [`engine`]: the simulation loop
//! - [`billing`]: tariff arithmetic over the simulated net-load series
//! - [`output`]: CSV/JSON result writers

pub mod billing;
pub mod engine;
pub mod milp;
pub mod output;
pub mod policy;
pub mod scenario;
pub mod state;

/// Convert an energy amount over one slot (kWh) to average power (kW).
pub fn kwh_to_kw(kwh: f64, slot_minutes: f64) -> f64 {
    kwh * 60.0 / slot_minutes
}

/// Convert average power (kW) sustained over one slot to energy (kWh).
pub fn kw_to_kwh(kw: f64, slot_minutes: f64) -> f64 {
    kw * slot_minutes / 60.0
}
