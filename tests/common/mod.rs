//! Shared scenario builders for the integration tests.
//!
//! Each test binary compiles this module separately, so any helper unused by
//! one binary would trip dead-code lints without the allow below.
#![allow(dead_code)]

use openv2b::scenario::{Charger, DrEvent, Manifest, Scenario, Vehicle};

pub fn manifest(horizon_slots: usize) -> Manifest {
    let json = format!(
        r#"{{"slot_minutes": 15.0, "horizon_slots": {horizon_slots},
             "demand_charge_usd_per_kw": 0.0}}"#
    );
    serde_json::from_str(&json).expect("manifest json is valid")
}

pub fn vehicle(id: u32, arrival: usize, departure: usize) -> Vehicle {
    Vehicle {
        vehicle_id: id,
        arrival_slot: arrival,
        departure_slot: departure,
        battery_kwh: 60.0,
        soc_arrival_kwh: 20.0,
        soc_target_kwh: 40.0,
        max_charge_kw: 20.0,
        max_discharge_kw: 20.0,
        min_soc_kwh: 0.0,
        max_soc_kwh: None,
        depletion_kwh: 0.0,
    }
}

pub fn charger(id: u32, bidirectional: bool) -> Charger {
    Charger {
        charger_id: id,
        max_kw: 20.0,
        bidirectional,
    }
}

/// A scenario with flat building load and price, and no DR events.
pub fn base_scenario(
    horizon_slots: usize,
    vehicles: Vec<Vehicle>,
    chargers: Vec<Charger>,
) -> Scenario {
    let scenario = Scenario {
        manifest: manifest(horizon_slots),
        vehicles,
        chargers,
        building_load_kw: vec![50.0; horizon_slots],
        price_usd_per_kwh: vec![0.20; horizon_slots],
        tou_class: vec![openv2b::scenario::TouClass::OffPeak; horizon_slots],
        dr_events: Vec::new(),
    };
    scenario.validate().expect("test scenario must be valid");
    scenario
}

pub fn dr_event(start: usize, end: usize, fsl_kw: f64) -> DrEvent {
    DrEvent {
        start_slot: start,
        end_slot: end,
        fsl_kw,
        penalty_usd_per_kwh: 6.0,
        incentive_usd_per_kw: 13.6,
        baseline_kw: 0.0,
    }
}
