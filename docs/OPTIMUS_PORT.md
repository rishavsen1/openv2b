# The OPTIMUS policy port: fidelity contract and divergence ledger

openv2b's heuristic policies (`policy-0`, `policy-1`, `policy-2`, `edf`, `llf`) are FAITHFUL
PORTS of the reference simulator's decision policies, transcribed rule-for-rule from a
line-anchored extraction of its source. The port's acceptance test is bill parity against the
reference on identical converted episodes, with every residual dollar attributed to a named,
explicitly ruled divergence. This document is the contract: if a port behavior looks odd, it
is either (a) the reference's behavior, reproduced deliberately, or (b) listed in the
divergence ledger below. Nothing in between is acceptable.

## Ported semantics (the load-bearing details)

- **Threshold (`historical_max_load`)** for `edf`/`llf`: seeded from the manifest's
  `heuristic_threshold_kw` (the converter performs the reference's monthly-percentile parquet
  lookup; SEP2024 = 117.14761373157317 kW) or the reference's own fallback,
  `0.8 x max(building series)`. It RATCHETS monotonically: after each decision, if
  `building + used_power` exceeds it, it is raised to that value for the rest of the episode.
- **Eligibility**: `edf`/`llf` use STRICT inequalities (peak TOU: SoC below target; otherwise:
  below the `max_soc` ceiling). `policy-0/1/2` use the toleranced predicates
  (`|a-b| <= 0.1pp + 1e-5|b|`, numpy's isclose). These differ in the reference too.
- **The EDF sort key is deadline PRESSURE**, `100 * need_kwh * max_charge_kw /
  ((threshold - building) * time_left_sec)`, descending, with IEEE inf/NaN semantics (NaN
  last). **The LLF sort key is raw `time_left`**, ascending: the reference never computes a
  laxity despite the name, and the port reproduces that as-is.
- **Budget walk**: capacity = threshold - building, computed once; top-of-loop `<= 0` break;
  the reference's exact clip arithmetic (the guard compares against the already-decremented
  capacity; copied verbatim, do not "fix"); clip to charger max; taper LAST; a car is marked
  served only if its post-taper rate >= its original need-rate.
- **Signed needs are the discharge channel**: off-peak eligibility admits above-target cars,
  whose negative need flows through the walk (and credits the budget) and through the
  force-charge fallback as a METERED discharge that lands the car on its target at departure.
- **Force-charge fallback**: eligible cars with `time_left < 3600 s` (hardcoded in the
  reference; no ini key exists) and not fully served get their full need-rate OUTSIDE the
  budget (still tapered), then feed the ratchet.
- **Taper** (`get_rate`): >90%-of-TRUE-capacity linear taper to zero at 100%, exact
  comparisons, ceiling cutoff at `max_soc`, hard discharge floor at `min_soc`, no shaping on
  negatives. The 90/100 anchors are percent of true capacity (`battery_kwh`), NOT of the
  vehicle's own ceiling: with the common 90% ceiling the taper is inert, as in the reference.
- **Charger assignment** (engine): arrivals in ascending vehicle id; EVERY car prefers a
  bidirectional port; a car finding no vacancy is dropped permanently (never retried) and
  reported `never_connected` at its departure.
- **POLICY_1's overlapping passes**: the discharge pass overwrites the charge pass, so
  off-peak a car above its target is STOPPED rather than charged toward the ceiling.
- **POLICY_2** charges only at `off-peak` exactly (super-off-peak gets nothing).
- **POLICY_3 is omitted**: its discharge leg calls a method that does not exist in the
  reference (`find_cars_above_req_soc` vs `find_cars_over_req_soc`), so it crashes on first
  use there and has no behavior to replicate. (Ruled: omit and document.)

## Parity result (RISHAV_WEEK/SEP2024, identical converted episodes)

| ep | reference LLF | openv2b LLF | delta | attribution |
|---|---|---|---|---|
| 1 | $3883.71 | $3884.47 | +$0.76 | +$1.10 (F-A) - $0.34 (F-G) |
| 2 | $3877.66 | $3878.38 | +$0.72 | fencepost - over-limit churn |
| 3 | $3930.38 | $3931.04 | +$0.66 | fencepost - over-limit churn |

Demand charges match to the cent on all episodes; net EV energy is identical (e.g. ep1:
325.0 kWh both). The oracle path reconciles the same way: with the degradation coefficient
matched (0.05, hardcoded in the reference; its `battery_deg_cost` ini key is dead config) and
the ramp added, oracle bills agree to about $0.01 modulo F-A.

## Divergence ledger (each with its ruling)

| id | reference behavior | openv2b behavior | ruling | measured impact |
|---|---|---|---|---|
| F-A | the billing pipeline never bills the episode's final 15-min interval (per-event elapsed time has no successor at the end) | bills every slot | keep openv2b correct; subtract explicitly in comparisons | +$1.10 to +$1.14 per week-episode |
| F-B | infeasible actions raise an exception and abort the run (three policies call the checker; the rest never do) | the engine clamps to physical limits, always | document divergence | none observed on parity data |
| F-C | the aggregate no-export check compares kWh to kW: 4x too permissive at 15-min slots | net site load >= 0, strictly | keep strict physics | none observed |
| F-D | the environment applies commanded rates without clamping SoC | hard clamps at floor/ceiling/capacity | keep strict physics | none observed (taper keeps rates near zero at bounds) |
| F-E | POLICY_3 crashes (typo'd method) | omitted, documented | omit | n/a |
| F-F | unstable quicksort tie order in sorts and assignment | stable sorts, explicit vehicle-id / charger-id tie-breaks | accepted divergence | none observed (ties rare on real data) |
| F-G | the budget walk clips only at the charge maximum, so discharge commands can exceed the charger's physical limit (observed: -28.0 kW on a +/-20 kW port; 8.22 kWh/week over-limit) | discharge clamped at min(vehicle, port) | strict physics (same family as F-C/F-D) | -$0.34 to -$0.48 per week-episode |

Also documented, not divergences: the reference's effective degradation coefficient is the
hardcoded 0.05 $/kWh (`battery_deg_cost = 0.01` in its ini is stored but never read), and its
`charge_segments` key is likewise dead; its MPC horizon is a 1-2 day midnight sawtooth
(`mpc_horizon_sec` is not ini-settable) with a full re-solve every 15 minutes while sessions
are live.

## Scenario-MPC parity (reports/BENCHMARK.md has the full tables)

`policy::scenario_mpc` matches the reference ILP-MPC structurally: K unnormalized SAA
scenarios sourced from historical episodes, non-anticipativity on the committed first slot,
the midnight-sawtooth 1-2 day horizon, ramp, deg 0.05, live 1e6 shortfall, and the `p_max`
realized-history ratchet. The load-bearing detail is const-7 SCENARIO CHAINING: sampled future
sessions of tracked identities continue the connected car's battery (terminal-energy variable
minus depletion). Without it the sampled futures inflate the planned peak into a free plateau
and bills blow up by $98-375/week; with it, two of three test episodes land within ~$1.5 of
the reference (the third differs by one 2 kW peak slot, scenario-noise class). Matched-config
runtime: ~6x faster (in-process HiGHS vs per-solve CPLEX CLI + Python model rebuild).

## Process rule (why this document exists)

An earlier openv2b version shipped different algorithms under the `edf`/`llf` names; the ~5%
bill gap surfaced only in a manual cross-simulator benchmark. Standing rule: reference logic
is ported verbatim; any deliberate change ships under a DIFFERENT name with a loud callout;
bill parity with attributed residuals is the acceptance test for every ported policy.
