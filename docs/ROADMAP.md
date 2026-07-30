# Roadmap

## v0.1 (current)
- [x] Discrete-event core: slotted time, arrival/departure/DR events
- [x] State model: vehicles, chargers, building load, grid prices
- [x] SoC dynamics with charge/discharge efficiency, V2B discharge
- [x] Policies: Uncontrolled, Earliest-Deadline-First, Least-Laxity-First (both V1G and V2B modes)
- [x] Billing: energy charge, demand charge, demand-response penalty/credit vs. a firm service level
- [x] CSV scenario input, JSON + CSV results output
- [x] Invariant test suite + hand-computed golden scenario

## v0.2
- [x] Session persistence: vehicle identities recur across days, battery state carries over
      (chaining, depletion, clamp accounting, banking metrics, persistence-off switch)
- [x] TOU price classes + two-component demand charge (facilities + peak-TOU)
- [x] Verification apparatus: month datasets (lossless/lossy/capped), independent Python
      referee re-simulating every heuristic policy, randomized property sweep, mutation kills
- [ ] Multi-building scenarios with a shared upstream power cap and a global allocation layer
- [ ] Scenario generator (synthetic arrivals from parametric distributions, seeded)
- [ ] ACN-Data importer (read Caltech ACN-Data JSON into openv2b scenarios)
- [ ] Differential-test harness vs. ACN-Sim for V1G scenarios (plan: `docs/ACNSIM_V2B_PLAN.md`)

## v0.3
- [x] Solver-agnostic `MilpBackend` layer: in-process HiGHS (`solver-highs` feature) +
      universal LP-file/CLI backend (CPLEX-verified); see `docs/SOLVER_DESIGN.md`
- [x] Receding-horizon MPC policy (pure LP, honest information set, engine-clamped)
- [ ] In-process Gurobi backend (`grb` crate) with warm starts
- [ ] FSL commitment optimization (firm level as a decision variable vs a no-DR baseline)
- [ ] MPC-vs-oracle parity suites (deficit AND surplus regimes, staggered departures, drift canary)
- [ ] Risk-aware variants (scenario sampling, CVaR objective)
- [ ] Negotiation layer: arrival-time offer menus, user choice models, contract billing

## Non-goals
- Dashboards / GUI visualization (results are plain CSV/JSON; plot them with anything)
- Power-flow simulation (couple to external tools instead)
- Python bindings before the core API stabilizes
