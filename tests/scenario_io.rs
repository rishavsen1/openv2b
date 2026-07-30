//! End-to-end test over the shipped example scenario: loading, validation
//! errors, and a full run of every policy.

use openv2b::engine::run;
use openv2b::policy::{self, POLICY_NAMES};
use openv2b::scenario::Scenario;
use std::path::PathBuf;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/one_day")
}

#[test]
fn example_scenario_loads() {
    let s = Scenario::load(&example_dir()).expect("example scenario must load");
    assert_eq!(s.vehicles.len(), 3, "example ships 3 vehicles");
    assert_eq!(s.chargers.len(), 2, "example ships 2 chargers");
    assert_eq!(s.building_load_kw.len(), 96, "series densified to horizon");
    assert_eq!(s.price_usd_per_kwh.len(), 96, "series densified to horizon");
    assert_eq!(s.dr_events.len(), 1, "example ships 1 DR event");
    // Step-and-hold densification: slot 23 still has the slot-0 value,
    // slot 24 picks up the new one.
    assert_eq!(s.price_usd_per_kwh[23], 0.10, "hold previous value");
    assert_eq!(s.price_usd_per_kwh[24], 0.20, "step at the row's slot");
}

#[test]
fn example_scenario_runs_under_every_policy() {
    let s = Scenario::load(&example_dir()).expect("example scenario must load");
    for name in POLICY_NAMES {
        let pol = policy::by_name(name).expect("registered policy");
        let results = run(&s, pol.as_ref());
        assert_eq!(results.slots.len(), 96, "one record per slot");
        assert_eq!(results.sessions.len(), 3, "one result per session");
        assert!(
            results.bill.total_usd.is_finite(),
            "policy {name} produced a non-finite bill"
        );
    }
}

#[test]
fn invalid_scenarios_are_rejected() {
    let mut s = Scenario::load(&example_dir()).expect("example scenario must load");
    s.vehicles[0].soc_arrival_kwh = 1000.0; // above capacity
    assert!(
        s.validate().is_err(),
        "arrival SoC above capacity must be rejected"
    );

    let mut s = Scenario::load(&example_dir()).expect("example scenario must load");
    s.vehicles[0].departure_slot = s.vehicles[0].arrival_slot; // empty session
    assert!(s.validate().is_err(), "empty session must be rejected");

    let mut s = Scenario::load(&example_dir()).expect("example scenario must load");
    s.manifest.charge_efficiency = 1.5;
    assert!(s.validate().is_err(), "efficiency above 1 must be rejected");
}
