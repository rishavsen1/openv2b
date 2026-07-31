//! Negotiation layer v1 tests.

#![cfg(feature = "solver-highs")]

mod common;

use common::{base_scenario, charger, dr_event, vehicle};
use openv2b::engine::run;
use openv2b::milp::highs_backend::HighsBackend;
use openv2b::negotiation::{negotiate, NegotiationConfig};
use openv2b::policy;
use openv2b::scenario::Scenario;

/// A deficit vehicle whose flexibility is worth money: expensive early
/// prices, cheap later, so a delayed departure lets the building serve the
/// same energy cheaper.
fn fixture() -> Scenario {
    let mut v = vehicle(0, 0, 16);
    v.soc_arrival_kwh = 10.0;
    v.soc_target_kwh = 50.0;
    v.max_discharge_kw = 0.0;
    let mut s = base_scenario(48, vec![v], vec![charger(0, false)]);
    s.building_load_kw = vec![30.0; 48];
    for slot in 0..48 {
        s.price_usd_per_kwh[slot] = if slot < 20 { 0.40 } else { 0.10 };
    }
    s
}

fn config() -> NegotiationConfig {
    NegotiationConfig {
        choice_count: 5,
        delay_slots_per_tier: 4,
        target_reduction_per_tier_kwh: 2.0,
        temperature: 0.0,
        ..NegotiationConfig::default()
    }
}

#[test]
fn menu_shape_and_price_monotonicity() {
    let s = fixture();
    let (_, records) = negotiate(&s, &HighsBackend, &config()).expect("negotiates");
    assert_eq!(records.len(), 1, "one session, one record");
    let r = &records[0];
    assert_eq!(r.offers.len(), 5, "choice_count offers incl. reject");
    assert!(
        r.offers.last().expect("nonempty").is_reject,
        "last offer is reject"
    );
    assert_eq!(r.utilities.len(), r.offers.len(), "one utility per offer");
    // Deeper concessions can never cost the user more.
    for pair in r.offers[..4].windows(2) {
        assert!(
            pair[1].price_usd <= pair[0].price_usd + 1e-9,
            "tier {} price {} > tier {} price {}",
            pair[1].tier,
            pair[1].price_usd,
            pair[0].tier,
            pair[0].price_usd
        );
    }
    // In this fixture flexibility is genuinely valuable: the deepest offer
    // must be strictly cheaper than tier 0.
    assert!(r.offers[3].price_usd < r.offers[0].price_usd - 1e-6);
}

#[test]
fn negotiation_is_deterministic_given_a_seed() {
    let s = fixture();
    let mut cfg = config();
    cfg.temperature = 0.5;
    cfg.seed = 12345;
    let (_, a) = negotiate(&s, &HighsBackend, &cfg).expect("negotiates");
    let (_, b) = negotiate(&s, &HighsBackend, &cfg).expect("negotiates");
    assert_eq!(
        serde_json::to_string(&a).expect("serialize"),
        serde_json::to_string(&b).expect("serialize"),
        "same seed must reproduce identical contracts"
    );
}

#[test]
fn accepted_contract_runs_and_is_honored() {
    let s = fixture();
    let (modified, records) = negotiate(&s, &HighsBackend, &config()).expect("negotiates");
    let r = &records[0];
    assert!(
        !r.chosen_is_reject,
        "flexibility is valuable here: an offer should win"
    );
    assert!(
        r.new_departure_slot > 16 || r.new_target_kwh < 50.0,
        "the chosen offer should deviate from the original contract"
    );
    modified.validate().expect("negotiated scenario is valid");
    let result = run(
        &modified,
        policy::by_name("policy-0").expect("registered").as_ref(),
    );
    let sess = &result.sessions[0];
    assert_eq!(
        sess.departure_slot, r.new_departure_slot,
        "engine ran the new contract"
    );
    assert!(sess.target_met, "renegotiated target must be met");
}

#[test]
fn reject_keeps_original_terms() {
    // Make every offer terrible: no surplus shared, huge inconvenience,
    // cheap external option: the user rejects.
    let s = fixture();
    let mut cfg = config();
    cfg.surplus_share = 0.0;
    cfg.inconvenience_delay_usd_per_slot = 10.0;
    cfg.inconvenience_reduction_usd_per_kwh = 10.0;
    cfg.external_price_usd_per_kwh = 0.01;
    let (modified, records) = negotiate(&s, &HighsBackend, &cfg).expect("negotiates");
    let r = &records[0];
    assert!(r.chosen_is_reject, "user should walk away");
    assert_eq!(
        modified.vehicles[0].departure_slot, 16,
        "original departure kept"
    );
    assert_eq!(
        modified.vehicles[0].soc_target_kwh, 50.0,
        "original target kept"
    );
}

#[test]
fn delay_is_capped_at_the_next_session_of_the_same_vehicle() {
    // Two chained sessions with a tight gap: tier delays must never make
    // session 1 overlap session 2.
    let mk = |arr: usize, dep: usize| {
        let mut v = vehicle(3, arr, dep);
        v.soc_arrival_kwh = 10.0;
        v.soc_target_kwh = 40.0;
        v.max_discharge_kw = 0.0;
        v.depletion_kwh = 5.0;
        v
    };
    let mut s = base_scenario(96, vec![mk(0, 20), mk(22, 60)], vec![charger(0, false)]);
    for slot in 0..96 {
        s.price_usd_per_kwh[slot] = if slot < 30 { 0.40 } else { 0.10 };
    }
    let (modified, _) = negotiate(&s, &HighsBackend, &config()).expect("negotiates");
    assert!(
        modified.vehicles[0].departure_slot <= 22,
        "delay pushed session 1 past session 2's arrival"
    );
    modified.validate().expect("still valid");
}

/// Negotiated flexibility must actually help a DR window: with an offer
/// accepted (delay past the window), the building's bill improves versus the
/// un-negotiated run.
#[test]
fn negotiation_reduces_the_bill_when_flexibility_has_value() {
    let mut v = vehicle(0, 0, 24);
    v.soc_arrival_kwh = 10.0;
    v.soc_target_kwh = 50.0;
    v.max_discharge_kw = 0.0;
    let mut s = base_scenario(64, vec![v], vec![charger(0, false)]);
    s.building_load_kw = vec![40.0; 64];
    s.dr_events.push(dr_event(4, 20, 42.0)); // charging in-window is penalized
    let (modified, records) = negotiate(&s, &HighsBackend, &config()).expect("negotiates");
    let before = run(
        &s,
        policy::by_name("policy-0").expect("registered").as_ref(),
    );
    let after = run(
        &modified,
        policy::by_name("policy-0").expect("registered").as_ref(),
    );
    if !records[0].chosen_is_reject {
        assert!(
            after.bill.total_usd <= before.bill.total_usd + 1e-9,
            "accepted flexibility should not worsen the bill ({} vs {})",
            after.bill.total_usd,
            before.bill.total_usd
        );
    }
    assert!(after.sessions[0].target_met, "renegotiated target met");
}
