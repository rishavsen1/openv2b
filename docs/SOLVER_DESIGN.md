# Solver-agnostic optimization layer (v0.3)

Decision (2026-07-30, with Rishav): NOT PuLP/CVXPY (Python model-build time and file+CLI
handoff dominate when an MPC re-solves thousands of times). Instead a 3-tier Rust design:

```
policy::mpc::Mpc ──> milp::Model (backend-neutral formulation: vars, bounds, linear
                     constraints, objective)
                        │
                        ├── milp::backends::HighsBackend      (in-process, cargo feature
                        │                                      `solver-highs`; MIT-licensed,
                        │                                      the open default; ~zero per-solve
                        │                                      overhead)
                        ├── milp::backends::GurobiBackend     (in-process via the `grb` crate,
                        │                                      feature `solver-gurobi`; warm
                        │                                      starts; needs a license)
                        └── milp::backends::LpCliBackend      (no deps: writes CPLEX-LP format,
                                                               invokes ANY solver CLI, parses its
                                                               solution file: CPLEX, Xpress,
                                                               HiGHS, CBC, SCIP, Hexaly...)
```

Key properties:

- The formulation code is written once against `trait MilpBackend { fn solve(&self, model:
  &Model) -> Result<Solution, SolveError>; }`. Switching solvers is a constructor argument.
- The core crate keeps ZERO solver dependencies: `LpCliBackend` is pure std (file write +
  subprocess), and in-process backends live behind cargo features.
- Per-solve overhead budget: in-process backends add microseconds; `LpCliBackend` adds
  ~100-500 ms per solve (process spawn + license check + file parse). For an MPC solving every
  slot of a month (2880 solves) that difference is minutes vs hours: use in-process for
  production sweeps, CLI for solver A/B checks and CPLEX parity.
- Determinism: the Model serializes variables/constraints in insertion order; LP files are
  byte-stable; backends must pin threads=1 and a fixed seed where the API allows.

## LP-file dialect and solution parsing

`LpCliBackend` writes the CPLEX-LP format (readable by cplex, gurobi_cl, highs, cbc, scip,
xpress). Solution parsing is solver-specific; the backend takes a `SolutionFormat` enum
(`CplexXml` for `cplex -c "read m.lp" "optimize" "write sol.sol"`, `GurobiSol` for
`gurobi_cl ResultFile=sol.sol m.lp`, `HighsRaw` for `highs --solution_file sol.sol m.lp`, or
`VarValueLines` for anything that can be shaped into plain `name value` lines).

## MPC policy (first cut)

Receding-horizon, honest information set: the MILP sees only currently-connected sessions plus
the public series (prices, building forecast, DR windows, site cap). No future arrivals.
Variables per connected session v and future slot s within v's window: signed charge energy
`c[v,s]` (kWh/slot), SoC `e[v,s]` in [floor, capacity]; aggregate `agg[s] = building + 4*sum c`
(kW); peak variables for both demand components; DR overflow `ov[s] >= (agg[s] - F) * dt` on
covered slots; shortfall slack `z[v] >= E_target - e[v, dep]` at a large penalty M; discharge
positive-part `d[v,s] >= -c[v,s]` priced at a small degradation cost so discharge is never free.

Objective: `sum_s price*dt-normalized energy + K_fac*p_max + K_peak*p_max_peak_tou +
K_dr*sum ov + M*sum z + k_deg*sum d`.

The policy executes only the first slot's `c[v, now]` (converted to kW) and re-solves next slot.
The engine still clamps everything, so a wrong solve can degrade cost but never break physics.

Validation gates (same discipline as the heuristics):
- deficit-regime and surplus-regime parity fixtures with staggered departures (the SPEC section
  8 warning: synchronized-departure toys hide information-loss bugs),
- drift canary: under perfect foresight (static scenario, no arrivals after t0), the planned
  peak must never jump upward between successive re-solves,
- cross-backend agreement: HiGHS vs CLI-CPLEX objective within tolerance on the same fixtures,
- all existing physical invariants hold with the MPC policy plugged into the property sweep.
