//! A fully hand-computed scenario: every dollar in the bill is derived on
//! paper in the comments. If this test fails, the engine's arithmetic changed.

mod common;

use approx::assert_abs_diff_eq;
use common::{base_scenario, charger, vehicle};
use openv2b::engine::run;
use openv2b::policy;

/// One vehicle, uncontrolled policy, lossless, 8 slots of 15 min.
///
/// Vehicle: arrives slot 0, departs slot 8, SoC 20 -> target 30 kWh,
/// max charge 20 kW (= 5 kWh per slot). Uncontrolled charges 20 kW in
/// slot 0 (SoC 25) and slot 1 (SoC 30), then 0.
///
/// Building: flat 10 kW. Price: flat 0.20 $/kWh. Demand rate 11.67 $/kW.
///
/// - Energy: building 10 kW * 8 slots * 0.25 h = 20 kWh; EV = 10 kWh.
///   Import 30 kWh * 0.20 = $6.00.
/// - Demand: peak net = 10 + 20 = 30 kW; 30 * 11.67 = $350.10.
/// - Total: $356.10.
#[test]
fn hand_computed_bill_uncontrolled() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 30.0;
    let mut scenario = base_scenario(8, vec![v], vec![charger(0, false)]);
    scenario.building_load_kw = vec![10.0; 8];
    scenario.price_usd_per_kwh = vec![0.20; 8];
    scenario.manifest.demand_charge_usd_per_kw = 11.67;

    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );

    assert_abs_diff_eq!(results.slots[0].ev_charge_kw, 20.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.slots[1].ev_charge_kw, 20.0, epsilon = 1e-9);
    for rec in &results.slots[2..] {
        assert_abs_diff_eq!(rec.ev_charge_kw, 0.0, epsilon = 1e-9);
    }
    assert_abs_diff_eq!(results.bill.energy_imported_kwh, 30.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.energy_usd, 6.00, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.peak_net_kw, 30.0, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.demand_usd, 350.10, epsilon = 1e-9);
    assert_abs_diff_eq!(results.bill.total_usd, 356.10, epsilon = 1e-9);
    assert_abs_diff_eq!(results.sessions[0].soc_departure_kwh, 30.0, epsilon = 1e-9);
    assert!(results.sessions[0].target_met, "vehicle must reach 30 kWh");
}

/// Same scenario with charge efficiency 0.8: the battery needs 10 kWh, so the
/// grid must supply 12.5 kWh. Slot 0: 20 kW -> 5 kWh drawn, 4 kWh stored
/// (SoC 24). Slot 1: 20 kW -> SoC 28. Slot 2: remaining 2 kWh needs 2.5 kWh
/// of grid energy = 10 kW -> SoC exactly 30. Total drawn: 12.5 kWh.
#[test]
fn hand_computed_efficiency_losses() {
    let mut v = vehicle(0, 0, 8);
    v.soc_arrival_kwh = 20.0;
    v.soc_target_kwh = 30.0;
    let mut scenario = base_scenario(8, vec![v], vec![charger(0, false)]);
    scenario.manifest.charge_efficiency = 0.8;
    scenario.building_load_kw = vec![10.0; 8];

    let results = run(
        &scenario,
        policy::by_name("uncontrolled")
            .expect("registered")
            .as_ref(),
    );
    let s = &results.sessions[0];
    assert!(s.target_met, "target reachable well within 8 slots");
    assert_abs_diff_eq!(s.soc_departure_kwh, 30.0, epsilon = 1e-6);
    assert_abs_diff_eq!(s.energy_drawn_kwh, 12.5, epsilon = 1e-6);
}
