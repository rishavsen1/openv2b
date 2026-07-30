# Validation plan

Three layers, from cheapest to most external. Layers 1 is implemented; 2 and 3 are the plan of
record and should land with v0.2.

## 1. First-principles tests (implemented, `tests/`)

Invariant tests derived from physics and tariff arithmetic (see SPEC section 7), a hand-computed
golden scenario where every dollar is derived on paper in the test's comments, and an example
scenario exercised under every registered policy. These are the tests that make the V2B path
trustworthy: energy conservation with asymmetric efficiencies, the no-export guard, the discharge
budget (V2B never sacrifices a departure target), and the `(start, end]` DR window convention are
each pinned individually.

Planned additions:
- property-based tests (randomized scenarios from a seeded generator; assert all invariants),
- a mutation pass: deliberately break each engine clamp and confirm at least one test fails.

## 2. Cross-validation against ACN-Sim (V1G subset)

[ACN-Sim](https://github.com/zach401/acnportal) (BSD-3-Clause) models unidirectional charging.
For scenarios expressible in both tools — no discharge, no DR, single site cap — the two
simulators must agree on delivered energy per session and on the aggregate load profile up to
their documented modeling differences (ACN-Sim's pilot-signal quantization and its
battery-tail model must be disabled/matched: use its ideal-battery mode).

Harness sketch: a converter emits the same synthetic scenario as an openv2b directory and an
ACN-Sim `SessionInfo` list; run uncontrolled and EDF in both; compare per-session energy (exact)
and per-slot aggregate (tolerance for tail effects). Results are reported here; ACN artifacts are
not committed.

## 3. Differential testing against reference simulators

Anyone with access to another V2B implementation (commercial or academic) can run the same
scenario in both and compare `summary.json` totals. Compared *numbers* may be published (facts
are not copyrightable); other tools' code, inputs, or raw output files must not be committed to
this repository. Known model-level differences to expect when comparing against typical
research simulators:

- openv2b clamps infeasible policy requests; some simulators raise errors instead.
- openv2b bills the all-slots peak for the demand charge until TOU-classed demand lands (v0.2).
- openv2b has no implicit DR commitment for non-enrolled buildings; some legacy tools apply a
  hardcoded default firm level to every building and overstate non-enrolled bills.
- Efficiencies default to lossless; set both to 1.0 when the reference is lossless.
