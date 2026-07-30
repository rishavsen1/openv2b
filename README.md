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
settlement, heuristic policies (idle, uncontrolled, EDF, LLF, and V2B variants with banking and
a provably target-safe discharge budget), and a receding-horizon **MPC** over a solver-agnostic
LP/MILP layer (in-process HiGHS via `--features solver-highs`; a dependency-free LP-file + CLI
backend drives CPLEX/Gurobi/Xpress/any solver, verified bill-identical against CPLEX 22.1).
A month-scale simulation solves in ~2 s including 2880 MPC re-solves.

Verification: 58 Rust tests (invariants, hand-computed goldens, randomized property sweep with
coverage assertions, mutation-kill suite) plus an independent Python referee that re-simulates
every heuristic policy from scratch and must agree slot-exactly; it runs in CI. See
`docs/VERIFICATION_PLAN.md` and `docs/VALIDATION.md`.

## Quick start

```bash
cargo build --release
cargo test
cargo run --release -- --scenario examples/one_day/scenario.json --policy edf --out results/
```

A scenario is a directory of CSV files (vehicles, chargers, building load, grid prices, DR events)
described by a small JSON manifest. See `examples/one_day/` and `docs/INPUT_FORMAT.md`.

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
