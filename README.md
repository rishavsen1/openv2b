# openv2b

A lightweight, open-source discrete-event simulator for **EV vehicle-to-building (V2B)** charging and
scheduling research, written in Rust with zero heavyweight dependencies.

`openv2b` simulates a building with EV chargers, a base electrical load, time-of-use grid prices, and
demand-response (DR) events. Vehicles arrive with an initial state of charge and a departure deadline;
a pluggable **decision policy** assigns charge (or discharge, V2B) power to each connected vehicle every
time slot. The simulator tracks energy flows, enforces physical limits, and produces an itemized bill
(energy, demand, and demand-response components).

## Why another EV charging simulator?

[ACN-Sim](https://github.com/zach401/acnportal) (BSD-3-Clause, Caltech) is the reference open-source
simulator for *unidirectional* (V1G) smart charging [Lee et al., 2021]. `openv2b` focuses on what
ACN-Sim does not model:

- **Bidirectional power (V2B/V2G)**: vehicles can discharge into the building to shave peaks and serve
  demand-response commitments.
- **Building coupling**: an inflexible base load plus the charger fleet behind a single utility meter,
  with demand charges computed on the *combined* peak.
- **Demand response / firm service level (FSL)**: buildings can enroll to cap their net load during DR
  windows and are penalized/credited against their commitment.
- **Session persistence**: the same vehicle identity can appear across days, carrying its battery state
  between sessions.

### Relationship to ACN-Sim: complement, cross-validated

`openv2b` is deliberately **not** built on ACN-Sim, for three reasons. First, scope: ACN-Sim models
EV-only aggregate current with no building load, no DR settlement, and no cross-day battery state;
V2B economics live exactly in those couplings. Second, throughput: a 30-day, 2880-slot episode runs
in ~3 ms under openv2b's heuristics and ~1.7 s under its per-slot MPC, which makes month-scale
optimization studies and large sweeps practical. Third, and most importantly, independence is the
verification strategy: because the two simulators share no code, agreement between them is evidence
of correctness rather than of a shared bug. A companion plugin (`acnportal-v2b`) extends unmodified
acnportal 0.3.3 with bidirectional EVSEs and replays openv2b scenarios through it; on every scenario
expressible in both tools (charge-only and V2B, contention, force-charge, heterogeneous ports,
asymmetric efficiencies), per-slot power and per-session energies agree to **max |delta| = 0.0**.

> Z. J. Lee, S. Sharma, D. Johansson, S. H. Low. "ACN-Sim: An Open-Source Simulator for Data-Driven
> Electric Vehicle Charging Research." *IEEE Transactions on Smart Grid* 12(6), 2021.
> arXiv:2012.02809.

## Status

v0.2-alpha. Functional and tested: the core engine (engine-enforced physics: power caps, SoC
floor/ceiling, site cap, no-export guard, adversarial-policy safety), session persistence with
cross-day SoC chaining and banking, TOU tariffs with two-component demand charges, DR/FSL
settlement, heuristic policies faithfully ported from the reference simulator
(policy-0/1/2 and the threshold-budget EDF/LLF; see docs/OPTIMUS_PORT.md, with bill parity
attributed to the cent), and a receding-horizon **MPC** over a solver-agnostic
LP/MILP layer (in-process HiGHS via `--features solver-highs`; a dependency-free LP-file + CLI
backend drives CPLEX/Gurobi/Xpress/any solver, verified bill-identical against CPLEX 22.1).
A month-scale simulation solves in ~2 s including 2880 MPC re-solves.

Verification: 58 Rust tests (invariants, hand-computed goldens, randomized property sweep with
coverage assertions, mutation-kill suite) plus an independent Python referee that re-simulates
every heuristic policy from scratch and must agree slot-exactly; it runs in CI. See
`docs/VERIFICATION_PLAN.md` and `docs/VALIDATION.md`.

## Quick start

```bash
cargo build --release --features solver-highs   # solver-free build: drop the feature
cargo test --features solver-highs
./target/release/openv2b --scenario examples/one_day --policy llf --out results/
```

A scenario is a directory of CSV files (vehicles, chargers, building load, grid prices, DR events)
described by a small JSON manifest. See `examples/one_day/` and `docs/INPUT_FORMAT.md`.

## Running each policy

`openv2b --scenario <dir> --policy <name> [--out <dir>] [--futures <dir,dir,...>]`

| policy | what it is | needs |
|---|---|---|
| `idle` | building-only baseline (no EV action); use for EV-vs-building cost attribution | - |
| `uncontrolled` | charge every car toward its target at max feasible power | - |
| `policy-0` | reference JIT: minimum constant rate that reaches the target by departure | - |
| `policy-1` | reference TOU: charge to ceiling off-peak/super-off-peak, discharge above-target cars at peak | bidirectional chargers for the discharge leg |
| `policy-2` | reference: charge to ceiling at off-peak ONLY (super-off-peak idles) | - |
| `edf` / `llf` | reference threshold-budget schedulers (deadline-pressure / time-left priority, force-charge, ratchet); DR-blind by design | optional `heuristic_threshold_kw` in scenario.json (default: 0.8 x max building load) |
| `oracle` | full-horizon solve-once plan (perfect foresight, persistence-coupled), replayed | `--features solver-highs` |
| `oracle-cplex` | same, solved through the CPLEX CLI | local CPLEX; `OPENV2B_CPLEX_BIN=/path/to/cplex` (defaults to a known local path) |
| `mpc` | deterministic receding-horizon LP (connected sessions only, no sampled futures) | `--features solver-highs` |
| `mpc-cplex` | same via the CPLEX CLI | CPLEX, `OPENV2B_CPLEX_BIN` |
| `scenario-mpc` | K-future SAA MPC (reference-matched: unnormalized scenarios, sawtooth horizon, ramp, peak-history ratchet, const-7 chained futures) | `--features solver-highs`; `--futures dir1,dir2,...` = converted historical episodes, disjoint from the test episode |
| `scenario-mpc-cplex` | same via the CPLEX CLI | CPLEX, `OPENV2B_CPLEX_BIN`, `--futures` |

Scenario-level knobs (scenario.json; full schema in `docs/INPUT_FORMAT.md`): `slot_minutes`,
`horizon_slots`, `charge_efficiency`/`discharge_efficiency`, `site_cap_kw` (engine-enforced),
`demand_charge_usd_per_kw` (facilities, all-slot peak), `demand_charge_peak_usd_per_kw`
(peak-TOU peak), `persistence` (cross-day SoC chaining on/off), `heuristic_threshold_kw`
(EDF/LLF budget seed), `dr_events_file` (omit for no demand response). Algorithm constants
(MPC lookahead, shortfall penalty, degradation cost, ramp, negotiation tiers/shares/seed) are
Rust config structs with reference-matched defaults (`MpcConfig`, `OracleConfig`,
`ScenarioMpcConfig`, `NegotiationConfig`); a config-file surface for them is on the roadmap.

## Tools

```bash
cargo run --release --bin gen_month                      # regenerate examples/one_month*
cargo run --release --features solver-highs --bin plan_fsl -- --scenario <dir>
                                                         # optimize DR firm-level commitments
cargo run --release --features solver-highs --bin negotiate -- --scenario <dir> [--seed N]
                                                         # arrival-time contract menus
python3 tools/convert_optimus.py <optimus_episode> <out> # reference-format episode -> scenario
python3 tools/referee.py <scenario> <out>                # independent verification of a run
python3 tools/run_verification.py                        # full campaign: referee + determinism
python3 tools/parity_optimus.py --test-episodes <ep> [<ep> ...] \
    --train-episodes <ep> ... --policies llf,oracle,mpc,scenario-mpc \
    [--reference-results <dir>]                          # cross-simulator parity harness
python3 tools/md2html.py reports/<file>.md               # report HTML
```

`parity_optimus.py` reruns the whole `reports/BENCHMARK.md` comparison with one command:
it converts reference-format episode directories, runs the requested policies over them (the
training episodes become `scenario-mpc`'s `--futures` pool), and prints bills, peaks, and
wall-clock runtimes. Given `--reference-results <dir>` (a tree of runs in the reference
simulator's own `summary_bldg_0.json` + `metrics.json` layout) it adds a per-episode delta
table with attribution columns: `F-A` (the final billing interval the reference never charges
for, computed exactly from the openv2b run) and `res = delta - F-A`, which is where F-G and
solver vertex choice live. The reference tree is optional: without it, or when no episode
matches, the harness says so and prints the openv2b table alone. It is deliberately not part
of CI, since it depends on data that is not in this repository.

`referee.py` also honors one opt-in manifest field, `planner_ramp_kwh_per_slot`: declare it
only for a run whose applied trajectory came from a single ramp-limited plan. A receding
controller re-plans every slot and its committed slots are tied only *within* one plan
(measured: 15 kW slot-to-slot swings from `scenario-mpc` under a 1.25 kWh/slot ramp), and the
heuristics have no ramp at all.

## Design

- **Discrete time, event-driven**: fixed slot length (default 15 min); arrivals/departures/DR
  boundaries are events; the environment integrates energy between events.
- **Policies are pure functions** of the observable state: `fn decide(&self, obs: &Observation) -> Vec<PowerSetpoint>`.
  Implement the `Policy` trait to add your own.
- **Determinism is a contract**: the same scenario and policy always produce byte-identical results.
  A test enforces it.
- **Invariants are tested, not assumed**: energy conservation, power caps, SoC bounds, and billing
  identities each have dedicated tests. See `tests/`.

## Provenance and license

`openv2b` is an independent, from-scratch implementation. It contains no code from any proprietary
simulator; the behavior it implements is standard published EV-charging physics and tariff arithmetic.
See `docs/PROVENANCE.md` for the project's clean-room policy.

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your
option. Contributions are accepted under the same dual license (see `CONTRIBUTING.md`).
