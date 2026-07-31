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

Open residual: ep2's +$25.16 is a single 2 kW difference in the realized weekly peak,
consistent with scenario-indexing/tie-break noise between the two stochastic controllers
(OPTIMUS permutes its 5 episodes with numpy seed 42 and commits its scenario-0; which episode
is "scenario 0" differs). Chasing it further means matching their seed-permutation order;
recorded as a known, bounded difference rather than hidden in a tolerance.
