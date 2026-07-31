# Handoff: state and open threads (2026-07-31)

Read `CLAUDE.md` first (living state doc), then this file. Repo is clean, all work pushed to
`github.com/rishavsen1/openv2b` (`main`). 73 tests green under `--features solver-highs`
(50 solver-free), clippy and fmt clean on both feature sets, 21-run month campaign green
under the referee plus two-process determinism.

## Policy inventory (there is no naming gap)

| policy | horizon | information set |
|---|---|---|
| `idle`, `uncontrolled` | none | current slot |
| `policy-0/1/2`, `edf`, `llf` | none (myopic) | current slot; faithful reference ports |
| **`oracle`** (+ `oracle-cplex`) | full episode, solve once, replay | **perfect foresight, by definition and by name. The only such policy.** This is the separate full-horizon MILP (the reference's ILP-BASE analog) |
| `mpc` (+ `mpc-cplex`) | receding, sawtooth | past-only building forecast, published tariff, announced DR, connected sessions |
| `scenario-mpc` (+ `-cplex`) | receding, sawtooth, K sampled futures | same, plus futures sampled from historical episodes |

`oracle` already fills the "separate MILP with perfect foresight" role, so `mpc` keeps its
name: it is a receding-horizon controller, not an oracle. Hard rule 0 in `CLAUDE.md` enforces
that `oracle` is the sole exception to the no-future-state rule.

Known exception to audit later: `negotiation` prices its offer menu with single-session
`solve_oracle` calls (`src/negotiation.rs:172`), so menu pricing sees the realized building
load. Contract pricing is arguably a planning problem that should use a forecast. Not yet
addressed.

## Three independent threads. Do not conflate them.

### Thread A: fold the ACN-Sim cross-validation plugin into `xval/`
- Plan: `docs/XVAL_FOLD_PLAN.md`, rebuilt around a native translation layer (no Python policy
  mirrors; openv2b setpoints replayed into ACN-Sim as an independent physics engine).
- Blocked on nothing except a go-ahead. Sequence is PR0 then PR1.
- **PR0** (standalone, justified on its own): add `requested_kw` (signed, pre-clamp,
  post-dedup) and `emission_index` to `TraceRecord`/`trace.csv`; document `max_soc_kwh` in
  `SPEC.md` and `INPUT_FORMAT.md` (currently documented nowhere, which would force the bridge
  author to read Rust and falsify the independence claim).
- **PR1**: the fold itself (18-step checklist in the plan).
- Plugin currently lives at `/home/rishav/acnportal-v2b`, 8 commits, never pushed, no remote.
  Its X1-X4 results (delta 0.0) were obtained against the PRE-PORT openv2b and are stale.

### Thread B: scenario-MPC vs the reference MPC, remaining divergence
- Setup: converted RISHAV_WEEK eps 1-3, five training episodes 50-54, reference's seed-42
  order (51, 54, 52, 50, 53). Scratch artifacts (converted episodes, probe crate, reference
  dumps) live in the session scratchpad and are disposable; regenerate with
  `tools/convert_optimus.py` and `tools/parity_optimus.py`.
- Done: input-level diff at one solve (ep1, slot 120, both ramp-free) proved building vectors,
  horizon, prices/TOU/peak-slot set, scenario-0 session table, live-SoC anchor, objective
  coefficients and `p_max_hist` all identical; found and fixed two real defects (phantom
  duplicate session; chained-session depletion source).
- Remaining: extend the reference dump to the FULL 24-session list per scenario and to the
  emitted constraint rows at slot 120, then diff. If those match, the conclusion is solver
  vertex selection on an identical LP, which is then provable rather than asserted.
- Full record: `reports/BENCHMARK.md`.

### Thread C: forecast quality
- Removing the deterministic MPC's perfect building foresight cost ~$500/week on RISHAV_WEEK
  (ep1 $3796.08 -> $4362.71), all via the demand charge, because daily persistence
  mis-predicts each day's peak.
- A better past-only forecast (e.g. same-weekday, or a short regression on recent days) should
  recover most of that. `Observation::building_forecast_kw` in `src/state.rs` is the single
  place to change.

## Other open items

- Publish the plugin repo (needs a GitHub URL) once Thread A lands.
- Config-file surface: `MpcConfig`/`OracleConfig`/`ScenarioMpcConfig`/`NegotiationConfig` are
  Rust structs with reference-matched defaults; no `[policy]` section in `scenario.json` yet,
  so scenario-MPC futures and thresholds are not input-reproducible.
- Multi-building; Gurobi in-process backend (needs a license); tariff ratchets; negotiation v2.
