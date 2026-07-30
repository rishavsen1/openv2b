//! Tariff arithmetic over the simulated net-load series.
//!
//! The bill decomposes as:
//! `total = energy + demand + dr_penalty - dr_incentive`
//!
//! - **Energy**: sum over slots of imported energy times the slot price.
//!   Exported energy (net load below zero) earns nothing: there is no
//!   feed-in compensation.
//! - **Demand**: the demand rate times the maximum slot-average net load
//!   over the billing period.
//! - **DR penalty**: for each demand-response event, the energy above the
//!   committed firm service level within the `(start, end]` window, times the
//!   event's $/kWh penalty rate.
//! - **DR incentive**: paid per kW of committed reduction below the event's
//!   baseline, only if the window was honored (zero overflow).

use crate::engine::SlotRecord;
use crate::kw_to_kwh;
use crate::scenario::{Scenario, TouClass};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DrSettlement {
    pub start_slot: usize,
    pub end_slot: usize,
    pub fsl_kw: f64,
    /// Energy above the firm service level inside the window, kWh.
    pub overflow_kwh: f64,
    pub penalty_usd: f64,
    pub incentive_usd: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Bill {
    pub energy_usd: f64,
    /// Total demand charge: facilities (all-slots peak) + time-related
    /// (peak-TOU-class peak) components.
    pub demand_usd: f64,
    pub demand_facilities_usd: f64,
    pub demand_peak_tou_usd: f64,
    pub dr_penalty_usd: f64,
    pub dr_incentive_usd: f64,
    pub total_usd: f64,
    /// Peak slot-average net load over the period, kW.
    pub peak_net_kw: f64,
    /// Peak slot-average net load over peak-TOU-class slots, kW.
    pub peak_net_peak_tou_kw: f64,
    /// Total energy imported from the grid, kWh.
    pub energy_imported_kwh: f64,
    pub dr_settlements: Vec<DrSettlement>,
}

/// Compute the itemized bill for a simulated run.
pub fn compute_bill(scenario: &Scenario, slots: &[SlotRecord]) -> Bill {
    let dt_min = scenario.manifest.slot_minutes;

    let mut energy_usd = 0.0;
    let mut energy_imported_kwh = 0.0;
    let mut peak_net_kw: f64 = 0.0;
    let mut peak_net_peak_tou_kw: f64 = 0.0;
    for rec in slots {
        let imported_kwh = kw_to_kwh(rec.net_kw.max(0.0), dt_min);
        energy_imported_kwh += imported_kwh;
        energy_usd += imported_kwh * rec.price_usd_per_kwh;
        peak_net_kw = peak_net_kw.max(rec.net_kw);
        if rec.tou == TouClass::Peak {
            peak_net_peak_tou_kw = peak_net_peak_tou_kw.max(rec.net_kw);
        }
    }
    let demand_facilities_usd = scenario.manifest.demand_charge_usd_per_kw * peak_net_kw;
    let demand_peak_tou_usd =
        scenario.manifest.demand_charge_peak_usd_per_kw * peak_net_peak_tou_kw;
    let demand_usd = demand_facilities_usd + demand_peak_tou_usd;

    let mut dr_penalty_usd = 0.0;
    let mut dr_incentive_usd = 0.0;
    let mut dr_settlements = Vec::new();
    for event in &scenario.dr_events {
        let covered_slots = slots.iter().filter(|rec| event.contains(rec.slot)).count();
        let overflow_kwh: f64 = slots
            .iter()
            .filter(|rec| event.contains(rec.slot))
            .map(|rec| kw_to_kwh((rec.net_kw - event.fsl_kw).max(0.0), dt_min))
            .sum();
        let penalty_usd = event.penalty_usd_per_kwh * overflow_kwh;
        // An incentive requires an actually-simulated, honored window;
        // validation already rejects out-of-horizon events, this is defense
        // in depth against a free incentive on an empty window.
        let honored = covered_slots > 0 && overflow_kwh <= 1e-9;
        let incentive_usd = if honored {
            event.incentive_usd_per_kw * (event.baseline_kw - event.fsl_kw).max(0.0)
        } else {
            0.0
        };
        dr_penalty_usd += penalty_usd;
        dr_incentive_usd += incentive_usd;
        dr_settlements.push(DrSettlement {
            start_slot: event.start_slot,
            end_slot: event.end_slot,
            fsl_kw: event.fsl_kw,
            overflow_kwh,
            penalty_usd,
            incentive_usd,
        });
    }

    Bill {
        energy_usd,
        demand_usd,
        demand_facilities_usd,
        demand_peak_tou_usd,
        dr_penalty_usd,
        dr_incentive_usd,
        total_usd: energy_usd + demand_usd + dr_penalty_usd - dr_incentive_usd,
        peak_net_kw,
        peak_net_peak_tou_kw,
        energy_imported_kwh,
        dr_settlements,
    }
}
