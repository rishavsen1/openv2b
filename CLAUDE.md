# CLAUDE.md

Guidance for Claude Code when working in this repository. **Update this file at every work
session**: keep "Current state" and "Active work" truthful, move finished items into the
CHANGELOG-style history at the bottom, and record any new invariant, convention, or gotcha the
moment it is established.

## What this is

`openv2b`: a clean-room, open-source discrete-event simulator for EV vehicle-to-building (V2B)
charging research, in Rust. Independent reimplementation from a written behavioral spec
(`docs/SPEC.md`); it must NEVER contain code, comments, or data fixtures copied from any
proprietary simulator (see `docs/PROVENANCE.md`). License: MIT OR Apache-2.0; commits use DCO
sign-off (`git commit -s`).

## Commands

```bash
cargo test                                   # 54 tests, no solver needed
cargo test --features solver-highs           # +4 MPC tests (in-process HiGHS)
cargo test --features solver-highs -- --ignored   # CPLEX CLI parity (needs local CPLEX)
cargo fmt --check && cargo clippy --all-targets -- -D warnings   # CI gates (run BOTH feature sets)
cargo run --release --bin gen_month          # regenerate examples/one_month*
cargo run --release -- --scenario examples/one_day --policy llf --out /tmp/out
python3 tools/referee.py examples/one_day /tmp/out        # independent verification
python3 tools/run_verification.py            # full campaign: 18+ runs, referee + determinism
python3 tools/parity_optimus.py --test-episodes <ep> --train-episodes <ep> \
    --policies llf,oracle,mpc,scenario-mpc [--reference-results <dir>]  # BENCHMARK.md rerun
python3 tools/md2html.py reports/OVERNIGHT_REPORT.md      # report HTML (never hand-edit .html)
```

## Architecture (one paragraph per layer)

- `src/scenario.rs`: input model + `validate()`. Validation REJECTS, never repairs (non-finite
  values, range violations, overlapping sessions/DR windows, duplicate ids/slots).
- `src/state.rs`: `Observation` (what policies see; sessions in canonical (arrival, vehicle_id)
  order), `SessionView`, `Setpoint` (signed kW; + charge grid-side, - discharge building-side).
- `src/engine.rs`: the simulation loop. Slot order: departures -> arrivals (persistence chain
  resolves here) -> reference charger assignment (ascending vehicle id, bidirectional ports
  first for EVERY car, unassignable cars dropped permanently) -> policy -> clamp & integrate. THE
  ENGINE OWNS FEASIBILITY: per-port caps, SoC floor/ceiling, site cap, no-export guard,
  non-finite setpoint rejection, one-setpoint-per-session. Scarce headroom is rationed in the
  POLICY'S EMISSION ORDER (never CSV row order).
- `src/policy/`: `Policy` trait (deterministic; per-episode instance state allowed, e.g. the
  EDF/LLF ratchet). Heuristics are FAITHFUL OPTIMUS PORTS (docs/OPTIMUS_PORT.md): policy-0/1/2
  and the threshold-budget edf/llf (parquet-or-fallback threshold, strict eligibility, signed
  needs as the discharge channel, taper-last, 1-hour force-charge bypass, monotone ratchet).
  POLICY_3 omitted (non-functional in the reference). idle/uncontrolled are openv2b-native
  baselines. `mpc.rs` (receding LP), `oracle.rs` (full-horizon, persistence-coupled, FSL
  optimization).
- `src/milp/`: solver-agnostic `MilpBackend` trait; `cli.rs` (LP-file + any solver CLI,
  CPLEX-verified), `highs_backend.rs` (in-process, feature `solver-highs`).
- `src/billing.rs`: energy on imports, two demand components (facilities all-slots peak +
  peak-TOU-class peak), DR settlement per event under the (start, end] convention.
- `tools/referee.py`: INDEPENDENT Python re-implementation (stdlib only, runs in CI). It
  re-simulates every heuristic policy from scratch and must agree with the engine slot-exactly.
  When you change policy/engine semantics you MUST mirror the change here, in the referee's own
  words, and the campaign must stay green. That duplication is deliberate: it is the
  differential oracle. Its POLICY-AGNOSTIC checks (they cover oracle/mpc/scenario-mpc, which
  are not re-simulated) are: per-DR-event settlement against the peak inside that window,
  per-session reconciliation of metered energies + occupancy window + SoC recursion against
  that session's OWN trace rows, and an OPT-IN ramp bound gated on the manifest field
  `planner_ramp_kwh_per_slot` (see hard rule 8).

## Hard rules (each one exists because a review found the violation)

0. NO PLANNER MAY READ FUTURE STATE. Policies may use: measured history, the current
   observation, the published tariff schedule, announced DR windows, contracted session
   terms of CONNECTED cars, and sampled/forecast futures. They may NOT index
   `Observation::building_series` beyond `obs.slot` (use `building_forecast_kw`), may not see
   the test episode's future sessions, and may not source a sampled session's depletion from
   the realized episode (`ScenarioMpcConfig::test_sessions` exists only to reproduce the
   reference's leak for comparison and must stay empty in normal runs). The `oracle` policy is
   the sole, clearly-named exception: perfect foresight is its definition.

1. The `(start, end]` DR window convention appears in 4 places (scenario.rs, engine.rs,
   billing.rs, referee.py) and is anchored by the hand-written table in
   `tests/dr_window_table.rs`. Change all or none.
2. Determinism is a contract: no wall clock, no unseeded randomness, byte-identical reruns,
   row-permutation invariance. The verification driver checks two-process SHA-256.
3. Heuristic policy logic is REPLICATED from OPTIMUS, never redesigned (2026-07-31 lesson: a
   silently substituted algorithm under the same name invalidated cross-simulator comparisons).
   Deviations require a different policy name plus a loud callout. Acceptance test for a port:
   bill parity vs the reference on RISHAV_WEEK, residuals attributed to named, ruled
   divergences (see docs/OPTIMUS_PORT.md).
4. Tests must make the guarded path BIND. The recurring review failure mode was tests passing
   "for the wrong reason" (geometry never exercised the guard). When adding a test, check what
   would happen under the mutation it is meant to kill.
5. New behavior flags default to legacy/off and must leave existing outputs byte-identical.
6. Never commit outputs from proprietary simulators as fixtures; synthetic or hand-computed only.
7. `reports/*.html` are generated (tools/md2html.py); edit only the .md sources.
8. A RECEDING controller's realized trajectory is NOT ramp-bounded, however hard its LP ramps.
   Consecutive committed slots come from two different solves and are tied only *within* each
   plan: measured on `scenario-mpc`, 15 kW slot-to-slot swings under a 1.25 kWh/slot (= 5 kW)
   ramp. So the referee's ramp check is opt-in via `planner_ramp_kwh_per_slot`, and that field
   belongs only to a run replayed from ONE ramp-limited plan. Never set it for a receding
   policy or a heuristic (which has no ramp at all).

## Current state (update me)

v0.4-alpha, 2026-07-31. THE OPTIMUS REPLICATION MILESTONE IS COMPLETE: heuristics are faithful
ports (docs/OPTIMUS_PORT.md is the fidelity contract + divergence ledger F-A..F-G with
rulings and dollar impacts); LLF/oracle/MPC bill parity on converted RISHAV_WEEK eps 1-3 is
attributed to the cent (reports/BENCHMARK.md); scenario-MPC (K=5, const-7 chained futures)
matches the reference within ~$1.5 on 2 of 3 episodes at ~6x speed. 73 tests green under
--features solver-highs, clippy/fmt clean both feature sets, referee re-simulates all seven
policies slot-exactly, 21-run month campaign green. Binaries: openv2b (policies incl.
oracle/mpc/scenario-mpc with --futures, CPLEX variants via OPENV2B_CPLEX_BIN), gen_month,
plan_fsl, negotiate. Known gaps: Gurobi backend (license), tariff ratchets, multi-building,
ACN-Data importer, negotiation v2, ep2 scenario-MPC +$25 (scenario-index noise, documented).

## Active work (update me)

See `docs/HANDOFF.md` for the full state and the three INDEPENDENT threads (do not conflate
them): A = fold the ACN-Sim plugin into `xval/` via a setpoint-replay translation layer
(`docs/XVAL_FOLD_PLAN.md`, PR0 trace fields then PR1); B = the remaining scenario-MPC vs
reference divergence (next step: dump the full per-scenario session list and the emitted
constraint rows at ep1 slot 120 and diff); C = a better past-only building forecast than daily
persistence (`Observation::building_forecast_kw`).

Policy inventory note: `oracle` IS the separate full-horizon MILP and is the only policy with
perfect foresight (hard rule 0). `mpc` is receding-horizon and forecast-based; it keeps its
name. Unaudited exception: `negotiation` prices menus with `solve_oracle`, so menu pricing
sees the realized building load (`src/negotiation.rs:172`).

## Clarified gaps vs OPTIMUS (2026-07-30 review with Rishav)

Missing elements, all additive at existing seams, none architectural:
- **OPTIMUS-format converter** (episodes -> scenario dirs): timestamps -> slot indices from the
  midnight origin, SoC percent x capacity/100 -> kWh, cars.csv + sessions.csv merge -> one
  vehicles.csv row per session, `charge_rates_kw` tuple -> max_kw + bidirectional, per-building
  split -> one scenario per building. ~100 lines, not written yet.
- **Multi-building** + a policies.csv equivalent (one scenario = one building today).
- **Ramp constraint** (`q_delta`) absent from both LPs.
- **Strict mode**: engine clamps infeasible actions by design; OPTIMUS raises. A
  raise-on-clamp flag would be a small addition if byte-faithful error behavior is wanted.
- **Settlement ledgers** (banked-energy revenue, replay credits), per-user-type inconvenience
  tables, CVaR scenario solves, RL policies, historical-threshold/LSTM budget variants.
- **Config surface**: scenario.json is the base.ini analog for environment/tariff; policy is
  chosen on the CLI; MpcConfig/OracleConfig/NegotiationConfig constants (lookahead, shortfall
  M, degradation, tiers, surplus share, temperature, seed) are exposed as Rust structs with
  defaults but NOT yet settable from a config file. TODO: a `[policy]`/`[negotiation]` section
  in scenario.json (or a run.toml) so full runs are reproducible from inputs alone.
- Hardcoded engine constants worth knowing: 1e-9 target-met tolerance, 1e-9 honored-window
  gate (both documented in SPEC and pinned by tests).

Measured runtime reference (this machine, month-scale, 30 days x 96 slots): openv2b heuristic
(llf) ~3.4 ms per full run including CSV I/O; MPC (2880 in-process HiGHS re-solves)
~1.7 s. OPTIMUS non-ILP month episodes recorded 18-41 s in their own metrics.json (~5 ms per
event), i.e. openv2b heuristics are ~4 orders of magnitude faster.

## Cross-validation status (ACN-Sim plugin)

`~/acnportal-v2b` (56 pytest tests, pinned acnportal==0.3.3 / numpy 1.26 / pandas 1.5.3 /
setuptools 80 on CPython 3.10; setuptools >= 81 breaks acnportal's pkg_resources import):
X1-X4 cross-validation vs the openv2b release binary all close at max |delta| = 0.0e0
(uncontrolled, EDF, LLF, and both V2B variants; charge, discharge, force-charge, capability-
aware assignment on heterogeneous ports, (start,end] window coverage, asymmetric
efficiencies). Non-vacuity was verified by mutation (removing the clamp-order shim diverges
X2 by 22 kWh). Deviations from docs/ACNSIM_V2B_PLAN.md are recorded in that repo's README;
open items: X5 billing parity, queueing scenarios refused by the bridge.

## Semantics notes for the new modules

- `policy::oracle`: full-horizon LP with perfect foresight; persistence chains are COUPLED
  (session k+1's opening SoC is the previous terminal variable minus depletion), which is what
  lets it bank across days. Scope checks reject charger contention and heterogeneous port
  limits rather than silently mismodeling them. The oracle's bill is NOT a lower bound over
  bills (unbilled degradation term); compare with `deg_slack`, see `tests/parity.rs`.
- FSL optimization is two solves (counterfactual no-DR baseline, then commitment with F as a
  variable in [0, baseline]) plus a gated post-adjustment, because billing pays the incentive
  all-or-nothing while the LP prices it linearly (short windows would overcommit otherwise).
- Negotiation v1 is a PRE-PASS in arrival order, priced by single-session oracle solves;
  approximations are documented at the top of `src/negotiation.rs`. Delays are capped at the
  vehicle's next arrival so chains never overlap; the reject option keeps original terms and
  is exempt from the inconvenience penalty; choice is seeded softmax (temperature 0 = argmax).

## History

- 2026-07-29: clean-room scaffold, core engine, heuristics, billing, first 16 tests.
- 2026-07-30 (overnight): persistence, TOU/demand billing, R1 plan review (25 findings, 5
  critical, all fixed), month datasets + independent referee + property sweep, R2 audits
  (emission-order arbitration critical, NaN validation, force-charge, banking; mutation kill
  rate to 100% of the committed list), solver layer (HiGHS + LP-CLI/CPLEX) + MPC. Report:
  `reports/OVERNIGHT_REPORT.md`.
- 2026-07-30 (session 2): published to github.com/rishavsen1/openv2b; oracle + parity suites +
  drift canary; FSL commitment planner; negotiation layer v1; ACN-Sim plugin started in its
  own repo.
- 2026-07-30/31 (reconciliation): Rishav flagged silently-substituted policy logic (memory:
  faithful-replication lesson). Line-anchored port spec extracted from OPTIMUS; oracle gap
  decomposed to the cent (deg 0.05 hardcoded there, final-slot billing fencepost F-A, ramp);
  faithful ports REPLACED the simplified policies (V2B overlay deleted); reference assignment
  semantics; max_soc ceiling split from capacity; referee mirrors the ports; new reference
  defect found (F-G: over-limit discharge, 8.22 kWh/wk); scenario-MPC built with const-7
  chained futures; full benchmark in reports/BENCHMARK.md. OPTIMUS bench configs audited
  (deg/battery_deg_cost dead-config split, mpc_horizon_sec not ini-settable, threshold parquet
  value 117.14761373157317 for SEP2024).
- 2026-07-31 (test coverage): referee gained policy-agnostic checks that also bind on the
  optimizing policies (per-DR-event settlement vs the in-window peak, per-session trace
  reconciliation incl. occupancy window and SoC recursion, opt-in ramp bound); new
  `tests/scenario_mpc.rs` (4 tests: non-anticipativity, const-7 chained banking,
  `building_from_futures`, K=3 determinism), the first mutation-verified by deleting the
  na_cp/na_cn block; `tools/parity_optimus.py` reruns the BENCHMARK.md comparison in one
  command and degrades gracefully with no reference data (not in CI). Finding recorded as
  hard rule 8: receding controllers escape their own ramp constraint.
