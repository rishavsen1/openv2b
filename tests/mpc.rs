//! MPC + solver-layer tests.
//!
//! The LP-writer tests always run. Solver-in-the-loop tests run under
//! `--features solver-highs` (in-process, hermetic). The CPLEX CLI parity
//! test is `#[ignore]` because it needs a local CPLEX install:
//!   cargo test --features solver-highs -- --ignored cplex

mod common;

use openv2b::milp::{Model, Sense};

#[test]
fn lp_writer_emits_deterministic_model() {
    let mut m = Model::default();
    let x = m.add_var("x", 0.0, 10.0, 1.0);
    let y = m.add_var("y", f64::NEG_INFINITY, f64::INFINITY, -2.0);
    m.add_constraint("c1", vec![(x, 1.0), (y, 3.0)], Sense::Le, 7.5);
    m.add_constraint("c2", vec![(y, 1.0)], Sense::Ge, -4.0);
    let a = m.to_lp();
    let b = m.to_lp();
    assert_eq!(a, b, "LP emission must be deterministic");
    assert!(a.starts_with("Minimize"), "LP header");
    assert!(a.contains(" c1:"), "constraint names serialized");
    assert!(a.contains("y free"), "free variable bounds");
    assert!(a.ends_with("End\n"), "LP trailer");
}

#[cfg(feature = "solver-highs")]
mod with_highs {
    use super::*;
    use approx::assert_abs_diff_eq;
    use common::{base_scenario, charger, dr_event, vehicle};
    use openv2b::engine::run;
    use openv2b::milp::highs_backend::HighsBackend;
    use openv2b::milp::{MilpBackend, SolStatus};
    use openv2b::policy;
    use openv2b::policy::mpc::{Mpc, MpcConfig};

    /// Hand-checkable LP: min x + 2y s.t. x + y >= 10, x <= 6 -> x=6, y=4, obj 14.
    #[test]
    fn highs_solves_a_hand_lp() {
        let mut m = Model::default();
        let x = m.add_var("x", 0.0, 6.0, 1.0);
        let y = m.add_var("y", 0.0, f64::INFINITY, 2.0);
        m.add_constraint("c", vec![(x, 1.0), (y, 1.0)], Sense::Ge, 10.0);
        let sol = HighsBackend.solve(&m).expect("solve");
        assert_eq!(sol.status, SolStatus::Optimal);
        assert_abs_diff_eq!(sol.values[0], 6.0, epsilon = 1e-6);
        assert_abs_diff_eq!(sol.values[1], 4.0, epsilon = 1e-6);
        assert_abs_diff_eq!(sol.objective, 14.0, epsilon = 1e-6);
    }

    fn mpc() -> Box<Mpc> {
        Box::new(Mpc::new(Box::new(HighsBackend), MpcConfig::default()))
    }

    /// TOU arbitrage: price low in slots 0..8, high after, bidirectional
    /// port. The optimal plan is full banking arbitrage: buy 40 kWh cheap
    /// (SoC 20 -> 60, the port limit over 8 slots), discharge 20 kWh against
    /// the building's expensive slots, land exactly on the 40 kWh target.
    /// EV-attributable energy cost = 40 * 0.10 - 20 * 0.40 = -$4.00
    /// (profitable; the $1 battery-wear term in the objective is worth paying
    /// for the $6 gross arbitrage margin).
    #[test]
    fn mpc_charges_in_cheap_slots() {
        let mut s = base_scenario(24, vec![vehicle(0, 0, 24)], vec![charger(0, true)]);
        s.building_load_kw = vec![10.0; 24];
        for slot in 0..24 {
            s.price_usd_per_kwh[slot] = if slot < 8 { 0.10 } else { 0.40 };
        }
        let m = run(&s, mpc().as_ref());
        assert!(m.sessions[0].target_met, "MPC must meet the target");
        assert_abs_diff_eq!(m.sessions[0].soc_departure_kwh, 40.0, epsilon = 1e-6);
        let building_cost = 2.0 * 10.0 * 0.10 + 4.0 * 10.0 * 0.40;
        let ev_cost = m.bill.energy_usd - building_cost;
        assert_abs_diff_eq!(ev_cost, -4.0, epsilon = 1e-6);
        let unc = run(
            &s,
            policy::by_name("uncontrolled")
                .expect("registered")
                .as_ref(),
        );
        assert!(
            m.bill.total_usd < unc.bill.total_usd,
            "MPC beats uncontrolled"
        );
    }

    /// DR window with a surplus vehicle: MPC discharges to hold the firm
    /// level and still meets the target.
    #[test]
    fn mpc_discharges_to_honor_dr_window() {
        let mut v = vehicle(0, 0, 24);
        v.soc_arrival_kwh = 55.0;
        v.soc_target_kwh = 30.0;
        let mut s = base_scenario(24, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 24];
        s.dr_events.push(dr_event(4, 12, 40.0));
        let m = run(&s, mpc().as_ref());
        assert!(m.sessions[0].target_met, "target holds");
        assert!(
            m.bill.dr_penalty_usd < 1e-6,
            "MPC should fully cover the 10 kW shortfall: penalty {}",
            m.bill.dr_penalty_usd
        );
        let edf = run(&s, policy::by_name("edf").expect("registered").as_ref());
        assert!(
            m.bill.total_usd <= edf.bill.total_usd + 1e-9,
            "MPC at least as good as heuristic"
        );
    }

    /// Physics safety: MPC through the engine obeys every invariant even with
    /// asymmetric efficiencies and a site cap.
    #[test]
    fn mpc_respects_engine_invariants() {
        let mut v1 = vehicle(0, 0, 40);
        v1.soc_arrival_kwh = 50.0;
        v1.soc_target_kwh = 30.0;
        let mut v2 = vehicle(1, 4, 30);
        v2.soc_arrival_kwh = 5.0;
        v2.soc_target_kwh = 45.0;
        let mut s = base_scenario(48, vec![v1, v2], vec![charger(0, true), charger(1, false)]);
        s.manifest.charge_efficiency = 0.92;
        s.manifest.discharge_efficiency = 0.94;
        s.manifest.site_cap_kw = Some(70.0);
        s.dr_events.push(dr_event(12, 20, 40.0));
        let r = run(&s, mpc().as_ref());
        for rec in &r.slots {
            assert!(rec.net_kw >= -1e-9, "no export");
            assert!(rec.net_kw <= rec.building_kw.max(70.0) + 1e-9, "site cap");
        }
        for sess in &r.sessions {
            let expected = sess.soc_arrival_kwh + 0.92 * sess.energy_drawn_kwh
                - sess.energy_exported_kwh / 0.94;
            assert_abs_diff_eq!(sess.soc_departure_kwh, expected, epsilon = 1e-6);
        }
    }

    /// Cross-backend parity: HiGHS in-process vs CPLEX CLI on the same
    /// fixture must agree on the realized bill. Needs a local CPLEX binary.
    #[test]
    #[ignore = "requires a local CPLEX install; run with -- --ignored"]
    fn cplex_cli_backend_matches_highs() {
        use openv2b::milp::cli::LpCliBackend;
        let cplex_bin = std::env::var("OPENV2B_CPLEX_BIN")
            .unwrap_or_else(|_| "/home/rishav/ibm/cplex/bin/x86-64_linux/cplex".into());
        let workdir = std::env::temp_dir().join("openv2b_cplex_test");
        let cli = Mpc::new(
            Box::new(LpCliBackend::cplex(cplex_bin, workdir)),
            MpcConfig::default(),
        );
        let mut v = vehicle(0, 0, 24);
        v.soc_arrival_kwh = 55.0;
        v.soc_target_kwh = 30.0;
        let mut s = base_scenario(24, vec![v], vec![charger(0, true)]);
        s.building_load_kw = vec![50.0; 24];
        s.dr_events.push(dr_event(4, 12, 40.0));
        let a = run(&s, mpc().as_ref());
        let b = run(&s, &cli);
        assert_abs_diff_eq!(a.bill.total_usd, b.bill.total_usd, epsilon = 1e-4);
    }
}
