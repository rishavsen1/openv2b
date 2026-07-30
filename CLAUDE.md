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

v0.3-alpha, 2026-07-30 (second session). 72 tests green under `--features solver-highs` (54
solver-free), clippy/fmt clean both feature sets, CI includes the referee. Month campaign: 19
runs (incl. MPC) all referee-verified + deterministic. MPC solves a 2880-slot month in ~1.7 s
(HiGHS in-process); LP-CLI backend verified bill-identical vs CPLEX 22.1
(/home/rishav/ibm/cplex). Landed since the overnight report: full-horizon oracle
(`policy::oracle`, persistence-coupled, replayable), MPC-vs-oracle parity suites for BOTH
regimes + the upward-peak drift canary (`tests/parity.rs`), FSL commitment optimization with
honored-gate post-adjustment (`plan_fsl` binary, `tests/fsl.rs`), negotiation layer v1
(`src/negotiation.rs`, `negotiate` binary, `tests/negotiation.rs`). Known gaps: Gurobi backend
(needs license), tariff ratchets/minimum-demand clauses, multi-building, ACN-Data importer,
in-loop (non-pre-pass) negotiation.

## Active work (update me)

- Negotiation v2 candidates: in-loop arrival hook (replace the pre-pass), settlement ledger
  outputs, menu pricing that sees concurrent EV load.
- ACN-Sim plugin follow-ups (in `~/acnportal-v2b`, separate repo, unpushed): X5 billing
  parity needs its `SeriesTariff` + `v2b_analysis` components; scenarios with queueing
  (`never_connected`) are refused by its bridge by design.

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
(llf / llf-v2b) ~3.4 ms per full run including CSV I/O; MPC (2880 in-process HiGHS re-solves)
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
