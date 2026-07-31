//! Mutable simulation state and the observation handed to policies.

use crate::scenario::Vehicle;

/// A vehicle currently plugged in.
#[derive(Debug, Clone)]
pub struct Session {
    /// Index into `Scenario::vehicles` for this session.
    pub vehicle_index: usize,
    /// Charger the session is assigned to (index into `Scenario::chargers`).
    pub charger_index: usize,
    /// Effective SoC at arrival (CSV value or persistence-chained), kWh.
    pub soc_arrival_kwh: f64,
    /// Current battery state of charge, kWh.
    pub soc_kwh: f64,
    /// Grid energy drawn for this session so far, kWh (post-meter, pre-efficiency).
    pub energy_drawn_kwh: f64,
    /// Energy exported to the building so far, kWh.
    pub energy_exported_kwh: f64,
}

/// What a policy is allowed to see when deciding slot `slot`.
#[derive(Debug)]
pub struct Observation<'a> {
    pub slot: usize,
    pub slot_minutes: f64,
    /// Inflexible building load this slot, kW.
    pub building_load_kw: f64,
    /// Grid price this slot, USD/kWh.
    pub price_usd_per_kwh: f64,
    /// TOU class of this slot.
    pub tou: crate::scenario::TouClass,
    /// Site power cap, kW, if any.
    pub site_cap_kw: Option<f64>,
    /// Charging efficiency in (0, 1]: grid kWh -> battery kWh.
    pub charge_efficiency: f64,
    /// Discharging efficiency in (0, 1]: battery kWh -> building kWh.
    pub discharge_efficiency: f64,
    /// Firm service level if a DR window covers this slot, kW.
    pub dr_fsl_kw: Option<f64>,
    /// Connected sessions with their static request data.
    pub sessions: Vec<SessionView<'a>>,
    /// Full price series (policies may look ahead; the environment is
    /// deterministic and prices are day-ahead information).
    pub price_series: &'a [f64],
    /// Full building-load series, kW (day-ahead forecast for planners).
    pub building_series: &'a [f64],
    /// Full TOU class series.
    pub tou_series: &'a [crate::scenario::TouClass],
    /// All demand-response events (public program information).
    pub dr_events: &'a [crate::scenario::DrEvent],
    /// Threshold seed for the EDF/LLF budget schedulers, kW (manifest).
    pub heuristic_threshold_kw: Option<f64>,
    /// Facilities demand rate, USD/kW on the all-slots peak.
    pub demand_charge_usd_per_kw: f64,
    /// Time-related demand rate, USD/kW on the peak-TOU-class peak.
    pub demand_charge_peak_usd_per_kw: f64,
}

/// A connected session as seen by a policy.
#[derive(Debug)]
pub struct SessionView<'a> {
    /// Stable key for setpoints: position of this session in `Observation::sessions`.
    pub index: usize,
    pub vehicle: &'a Vehicle,
    pub soc_kwh: f64,
    /// Effective charge limit this slot, kW: min(vehicle, charger port).
    pub max_charge_kw: f64,
    /// Effective discharge limit this slot, kW: min(vehicle, charger port),
    /// 0 unless both vehicle and charger support it.
    pub max_discharge_kw: f64,
}

impl SessionView<'_> {
    /// Energy still needed to reach the departure target, kWh (battery side).
    pub fn remaining_need_kwh(&self) -> f64 {
        (self.vehicle.soc_target_kwh - self.soc_kwh).max(0.0)
    }

    /// Slots left before departure, counting the current slot.
    pub fn slots_to_departure(&self, slot: usize) -> usize {
        self.vehicle.departure_slot.saturating_sub(slot)
    }

    /// Laxity in slots: time remaining minus time needed at full charge power.
    /// Negative laxity means the target is no longer reachable.
    pub fn laxity_slots(&self, slot: usize, slot_minutes: f64, charge_efficiency: f64) -> f64 {
        let need_kwh = self.remaining_need_kwh();
        let per_slot_kwh = crate::kw_to_kwh(self.max_charge_kw, slot_minutes) * charge_efficiency;
        let slots_needed = if need_kwh <= 0.0 {
            0.0
        } else if per_slot_kwh <= 0.0 {
            f64::INFINITY
        } else {
            need_kwh / per_slot_kwh
        };
        self.slots_to_departure(slot) as f64 - slots_needed
    }
}

/// A policy's decision for one session in one slot.
/// Positive = charge (grid -> battery), negative = discharge (battery -> building), kW.
#[derive(Debug, Clone, Copy)]
pub struct Setpoint {
    pub session_index: usize,
    pub power_kw: f64,
}
