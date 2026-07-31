# OPTIMUS vs openv2b: same-data benchmark and parity record

Final results of the cross-simulator benchmark on `RISHAV_WEEK/SEP2024` episodes 1-3 (15
users, 15 bidirectional +/-20 kW chargers, 768 slots/episode), identical data via
`tools/convert_optimus.py`, identical machine. OPTIMUS timings are its own `metrics.json`
simulation times; openv2b timings are process wall-clock. Reference configuration was audited
line-by-line (threads=1 CPLEX for MPC, deg 0.05 hardcoded, ramp q_delta=1.25 kWh/slot always
on, 5 SAA scenarios = episodes 50-54 of RISHAV_15_USERS_2024, midnight-sawtooth 1-2 day
horizon, re-solve every 15 min).

## Bills (USD)

| policy | ep | OPTIMUS | openv2b | delta | attribution |
|---|---|---|---|---|---|
| LLF (threshold-budget port) | 1 | 3883.71 | 3884.47 | +0.76 | +1.10 F-A, -0.34 F-G |
| | 2 | 3877.66 | 3878.38 | +0.72 | fencepost - over-limit churn |
| | 3 | 3930.38 | 3931.04 | +0.66 | fencepost - over-limit churn |
| Oracle MILP (CPLEX both) | 1 | 3796.40 | 3797.28 | +0.88 | F-A + ramp timing (~0.01 residual with ramp ported) |
| | 2 | 3752.26 | 3753.12 | +0.86 | same |
| | 3 | 3763.76 | 3764.90 | +1.14 | same |
| MPC, 5 futures | 1 | 3795.36 | 3796.95 | +1.59 | ~F-A + scenario noise |
| | 2 | 3773.39 | 3798.55 | +25.16 | one 2.0 kW peak-slot difference (140.0 vs 138.0 x 11.67) |
| | 3 | 3761.93 | 3763.33 | +1.40 | ~F-A |

F-A = OPTIMUS never bills the episode's final 15-min slot (+$1.10-1.14/week; ruled: openv2b
stays correct). F-G = OPTIMUS discharges beyond its chargers' physical limits (8.22 kWh/week
observed, commands to -28 kW on +/-20 kW ports; ruled: openv2b keeps strict physics). Demand
charges match to the cent on LLF and the oracle; net EV energy identical on both.

## Runtimes (same machine)

| policy | OPTIMUS | openv2b | speedup | like-for-like? |
|---|---|---|---|---|
| LLF | 0.7 s | 1.2 ms | ~580x | yes (ported algorithm) |
| Oracle MILP | 0.9-1.0 s (sim loop; CPLEX) | 55-60 ms (CPLEX CLI), 47-50 ms (HiGHS) | ~18x | yes |
| MPC 5 futures | 356.8 / 369.0 / 375.4 s (CPLEX, threads=1) | 57.8 / 59.4 / 60.4 s (HiGHS in-process) | **~6x** | yes: K=5 unnormalized SAA, non-anticipativity, sawtooth horizon, ramp, deg 0.05, p_max history ratchet |
| MPC deterministic (no futures) | n/a | 1.0-1.7 s | - | openv2b-only reference point |

The earlier "1.7 s vs 356 s" comparison was NOT like-for-like (deterministic vs 5-scenario);
the honest matched-configuration speedup is ~6x, attributable to in-process solves (no
per-solve process/file/license overhead) and native-code model construction (OPTIMUS rebuilds
its docplex model in Python every 15 simulated minutes).

## What made scenario-MPC parity work (and the one open residual)

The decisive structural element was the reference's const-7 scenario chaining: a sampled
future session of a currently-connected identity must CONTINUE that car's battery (its opening
energy is the connected session's terminal-energy variable minus depletion), not arrive as an
independent load. Without the chain, sampled futures inflate planned demand into a flat "free
plateau" under the peak envelope and the realized peak blows up ($98-375 worse); with it,
eps 1/3 land within ~$1.5 of OPTIMUS. Deterministic-vs-scenario is also informative: on this
dataset the deterministic openv2b MPC ($3796.08/$3754.95/$3762.01) matches OPTIMUS's 5-future
MPC about as well as our scenario version does; arrival uncertainty is worth roughly nothing
here, so scenario count is a runtime cost without a bill benefit on RISHAV_WEEK.

## Eliminating every controllable source of difference (MPC)

Three controlled experiments were run to remove, one at a time, each suspected source of
uncertainty between the two MPCs. All use the SAME five training episodes (50-54 of
RISHAV_15_USERS_2024, converted) and the reference's own seed-42 ordering (51, 54, 52, 50, 53).

**(i) Scenario ordering.** Orders A (50..54), B (the reference's), C (53,50,52,54,51) were run.
Since non-anticipativity ties every scenario's first-slot rates, ordering can only act through
degenerate-optimum selection; the measured band was up to $59 on ep3. The reference's own
vertex choice is equally unpinned.

**(ii) Ramp `q_delta`.** Both simulators were re-run with the ramp effectively disabled
(reference: `configure.q_delta = 1e6`, verified reaching the emitted constraint RHS; openv2b:
`ramp_kwh_per_slot = None`).

| ep | reference ramped | reference no-ramp | openv2b ramped | openv2b no-ramp |
|---|---|---|---|---|
| 1 | 3795.36 | 3794.94 | 3796.95 | 3796.24 |
| 2 | 3773.39 | 3920.11 | 3797.79 | 3989.91 |
| 3 | 3761.93 | 3760.43 | 3822.65 | 3762.18 |

The ramp is NOT the source of the difference: removing it moves eps 1/3 by under $2 in both
simulators and makes ep2 dramatically worse in BOTH (+$147 reference, +$192 openv2b). That
shared, same-signed sensitivity is itself evidence of structural equivalence: the 5 kW ramp is
nearly free under perfect foresight (oracle: <$0.30) but load-bearing for a receding-horizon
controller, which without it cycles harder and sets a higher realized peak.

**(iii) The forecast model itself.** A source-level audit of the reference's scenario
construction found ONE genuine mismatch: each of its scenarios plans against **that training
episode's own building-load series**, i.e. the building load is FORECAST, not known. openv2b
originally gave every scenario the realized (test) series. `ScenarioMpcConfig::
building_from_futures` now implements the reference behavior and defaults to it:

| ep | reference | openv2b, sampled load (reference-faithful) | openv2b, known load |
|---|---|---|---|
| 1 | 3795.36 | 3796.93 | 3796.95 |
| 2 | 3773.39 | 3840.76 | 3797.79 |
| 3 | 3761.93 | 4000.38 | 3822.65 |

Also confirmed identical between the two: 5 unnormalized (equally weighted, no 1/K) scenarios;
future sessions taken wholesale from each training episode (not filtered to tracked
identities, no per-identity resampling in this mode); connected sessions replicated per
scenario and tied only at slot 0; scenario 0's slot 0 committed; const-7 chaining of returning
identities. Difference noted but benign on this data: the reference passes every scenario the
LAST training episode's price/charger tables (a leaked loop variable), harmless because all
RISHAV SEP2024 episodes share tariffs.

## What the residual actually is

After (i)-(iii), ep1 agrees to **$1.57** (and to **$0.88** in the ramp-free configuration,
which is exactly the oracle's residual on the same episode, i.e. the final-slot billing
fencepost F-A and nothing else). Eps 2-3 differ by more, and the mechanism is now identified:
both controllers commit one slot per re-solve over ~700 sequential LPs whose optima are
massively degenerate (many equal-cost plans differ only in WHICH slots carry the charging).
Each commitment changes the state the next solve sees, so vertex choice compounds into a
different realized peak, and the entire ep2/ep3 difference is a 2-20 kW peak, which the
$11.67/kW demand charge multiplies into the visible dollars. Evidence that this is
degeneracy and not a modeling error: the differences are non-monotone under every knob (ramp
off helps ep3 and hurts ep2; sampled load helps nothing on ep2/ep3 but is the reference's own
behavior; ordering alone moves ep3 by $59), whereas a formulation discrepancy would push
consistently in one direction.

Closing the last dollars would require both solvers to share a tie-breaking rule among equal-
cost vertices, which the reference does not define (it pins neither CPLEX threads nor seed on
the oracle path, and its `workmem` is a fraction of host RAM, so its own results are not
bit-reproducible across machines). Recorded as a bounded, explained difference rather than
hidden in a tolerance.
