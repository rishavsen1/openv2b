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
cargo run --release -- --scenario examples/one_day --policy edf-v2b --out /tmp/out
python3 tools/referee.py examples/one_day /tmp/out        # independent verification
python3 tools/run_verification.py            # full campaign: 18+ runs, referee + determinism
python3 tools/md2html.py reports/OVERNIGHT_REPORT.md      # report HTML (never hand-edit .html)
```

## Architecture (one paragraph per layer)

- `src/scenario.rs`: input model + `validate()`. Validation REJECTS, never repairs (non-finite
  values, range violations, overlapping sessions/DR windows, duplicate ids/slots).
- `src/state.rs`: `Observation` (what policies see; sessions in canonical (arrival, vehicle_id)
  order), `SessionView`, `Setpoint` (signed kW; + charge grid-side, - discharge building-side).
- `src/engine.rs`: the simulation loop. Slot order: departures -> arrivals (persistence chain
  resolves here) -> capability-aware charger assignment -> policy -> clamp & integrate. THE
  ENGINE OWNS FEASIBILITY: per-port caps, SoC floor/ceiling, site cap, no-export guard,
  non-finite setpoint rejection, one-setpoint-per-session. Scarce headroom is rationed in the
  POLICY'S EMISSION ORDER (never CSV row order).
- `src/policy/`: `Policy` trait (pure, deterministic), heuristics (idle, uncontrolled, EDF,
  LLF, +V2B variants with off-peak banking, surplus-only discharge budget, force-charge
  fallback), and `mpc.rs` (receding-horizon LP, honest information set).
- `src/milp/`: solver-agnostic `MilpBackend` trait; `cli.rs` (LP-file + any solver CLI,
  CPLEX-verified), `highs_backend.rs` (in-process, feature `solver-highs`).
- `src/billing.rs`: energy on imports, two demand components (facilities all-slots peak +
  peak-TOU-class peak), DR settlement per event under the (start, end] convention.
- `tools/referee.py`: INDEPENDENT Python re-implementation (stdlib only, runs in CI). It
  re-simulates every heuristic policy from scratch and must agree with the engine slot-exactly.
  When you change policy/engine semantics you MUST mirror the change here, in the referee's own
  words, and the campaign must stay green. That duplication is deliberate: it is the
  differential oracle.

## Hard rules (each one exists because a review found the violation)

1. The `(start, end]` DR window convention appears in 4 places (scenario.rs, engine.rs,
   billing.rs, referee.py) and is anchored by the hand-written table in
   `tests/dr_window_table.rs`. Change all or none.
2. Determinism is a contract: no wall clock, no unseeded randomness, byte-identical reruns,
   row-permutation invariance. The verification driver checks two-process SHA-256.
3. V2B heuristics may only discharge surplus above max(target, floor); MPC may borrow below
   target because its LP proves recovery. The referee enforces the first for heuristics only.
4. Tests must make the guarded path BIND. The recurring review failure mode was tests passing
   "for the wrong reason" (geometry never exercised the guard). When adding a test, check what
   would happen under the mutation it is meant to kill.
5. New behavior flags default to legacy/off and must leave existing outputs byte-identical.
6. Never commit outputs from proprietary simulators as fixtures; synthetic or hand-computed only.
7. `reports/*.html` are generated (tools/md2html.py); edit only the .md sources.

## Current state (update me)

v0.3-alpha, 2026-07-30. 58 tests green, clippy/fmt clean both feature sets, CI includes the
referee. Month campaign: 19 runs (incl. MPC) all referee-verified + deterministic. MPC solves a
2880-slot month in ~1.7 s (HiGHS in-process); LP-CLI backend verified bill-identical vs CPLEX
22.1 (/home/rishav/ibm/cplex). Known gaps: Gurobi backend (needs license), tariff ratchets,
multi-building, ACN-Data importer.

## Active work (update me)

- MPC-vs-oracle parity suites: deficit AND surplus regimes, staggered departures, drift canary
  (planned peak must never jump upward under perfect foresight).
- FSL commitment optimization (firm level as decision variable vs counterfactual baseline).
- Negotiation layer v1 (arrival-time offer menus, seeded choice model, contract settlement).
- ACN-Sim cross-validation plugin: SEPARATE repo (`~/acnportal-v2b`), plan in
  `docs/ACNSIM_V2B_PLAN.md`. openv2b stays standalone; the plugin exists only for
  cross-validation.

## History

- 2026-07-29: clean-room scaffold, core engine, heuristics, billing, first 16 tests.
- 2026-07-30 (overnight): persistence, TOU/demand billing, R1 plan review (25 findings, 5
  critical, all fixed), month datasets + independent referee + property sweep, R2 audits
  (emission-order arbitration critical, NaN validation, force-charge, banking; mutation kill
  rate to 100% of the committed list), solver layer (HiGHS + LP-CLI/CPLEX) + MPC. Report:
  `reports/OVERNIGHT_REPORT.md`.
