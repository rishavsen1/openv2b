# Plan: Extending ACN-Sim (acnportal) from V1G to V2B for cross-validation of openv2b

Verified against a fresh shallow clone of acnportal (v0.3.3, master @ `14e723b`, 2023-11-21),
the GitHub API for maintenance status, and `docs/SPEC.md` for the target semantics. Everything
below cites real files/classes that were read; unverified items are marked.

## 1. Architecture summary of ACN-Sim as found (v0.3.3)

**Core loop** (`acnportal/acnsim/simulator.py`, `Simulator.run()`): pop events for the current
iteration, run the scheduler if resolve is flagged, write the schedule matrix into
`pilot_signals`, call `network.update_pilots(...)`, record `charging_rates`, update `peak`,
increment iteration. Time is uniform periods of `period` minutes (any value; 15 works).

**Units**: pilot signals are **Amps per EVSE**; each EVSE has a voltage and phase angle
registered with the network (`ChargingNetwork.register_evse`). Power appears only at the battery
boundary (`pilot * voltage / 1000` kW) and in analysis (`aggregate_power = voltages .
charging_rates / 1000`).

**Key classes**:
- `Simulator` (`acnsim/simulator.py`): orchestration; accepts a pluggable `interface_type`
  constructor arg (line 51) registered with the scheduler; `signals` is a free-form dict (only
  `signals["tariff"]` is ever read, in `Interface.get_prices`).
- `ChargingNetwork` (`acnsim/network/charging_network.py`): ordered dict of EVSEs; constraints
  are rows of a `constraint_matrix` over station currents with scalar `magnitudes` limits.
  `constraint_current()` computes the complex phasor sum; `is_feasible()` checks
  `|aggregate| <= magnitude`. Has an explicit extension hook: `post_charging_update()` (line
  543, a documented no-op).
- `BaseEVSE / EVSE / DeadbandEVSE / FiniteRatesEVSE` (`acnsim/models/evse.py`): `set_pilot()`
  validates via `_valid_rate()` then forwards to the EV. `EVSE._valid_rate` enforces
  `min_rate <= pilot <= max_rate` with `min_rate` defaulting to 0: **negative pilots are
  structurally rejected here**, not deeper in the stack. This is the single choke point for
  V1G-only behavior.
- `Battery / Linear2StageBattery` (`acnsim/models/battery.py`): `Battery.charge(pilot, voltage,
  period)` computes `charge_power = min(pilot*V/1000, max_power, rate_to_full)`. A negative
  pilot would pass straight through that `min()` and decrease `_current_charge` with **no floor
  at 0 and no discharge power limit**; it is only safe today because EVSEs reject negative
  pilots upstream. `Linear2StageBattery._charge` warns on `pilot < 0` (line 247) and its
  exponential-tail math is not valid for discharge.
- `EV` (`acnsim/models/ev.py`): tracks `_energy_delivered` (net), `remaining_demand`,
  `fully_charged`. No SoC exposure to algorithms.
- `Interface / SessionInfo / InfrastructureInfo` (`acnsim/interface.py`): what algorithms see.
  `SessionInfo` carries requested/delivered energy, arrival/departure, and per-period
  `min_rates`/`max_rates` (already `Union[float, List[float]]`, so signed lower bounds are
  representable). **No building load, no DR concept anywhere** (zero grep hits).
- Events (`acnsim/events/`): `PluginEvent` (precedence 10), `UnplugEvent` (precedence 0),
  `RecomputeEvent` (precedence 20); heapq pops lowest `(timestamp, precedence)` first, so
  unplugs precede same-timestep plugins, matching openv2b's departures-before-arrivals, and the
  EV occupies `[arrival, departure)` exactly like openv2b's half-open session.
- Tariffs (`signals/tariffs/tou_tariff.py`): `TimeOfUseTariff` is calendar-JSON based;
  duck-typed via `get_tariffs(start, length, period)` and `get_demand_charge(datetime)`.
- Analysis (`acnsim/analysis/__init__.py`): free functions over a completed sim:
  `aggregate_power`, `energy_cost`, `demand_charge`, `proportion_of_demands_met`. All EV-only;
  no site netting.
- Precedent for out-of-tree extension: `acnportal/contrib/acnsim/network/stochastic_network.py`
  ships as a contrib subclass of `ChargingNetwork`.

## 2. Gap analysis vs the openv2b V2B semantics (SPEC.md)

| openv2b requirement | ACN-Sim status |
|---|---|
| Signed setpoints (kW, + charge / - discharge) | Pilots are Amps; sign blocked only by `EVSE._valid_rate` (`min_rate=0`). Simulator core is sign-agnostic numpy; `is_feasible` uses magnitudes of phasor sums, physically correct for reversed current. Constraint set needs no change in the default phasor mode. Caveat: `constraint_current(linear=True)` computes `np.abs(M @ schedule)` (charging_network.py:472-475) without taking abs of coefficients as its docstring claims; with mixed-sign schedules this permits cancellation and under-constrains. V2B feasibility must use the phasor path. |
| Battery discharge with SoC floor + directional limits + efficiencies | Absent. `Battery.charge` has no floor clamp, no `max_discharge_power`, no eta_c/eta_d. `Linear2StageBattery` is charge-only by construction. |
| Building base load behind the meter | Absent everywhere. `signals` dict is the natural free-form carrier (only `"tariff"` is reserved). |
| No-export guard (net site load >= 0, engine-authoritative, charge-first-then-discharge clamp order) | Absent. ACN warns on infeasible schedules but applies them anyway; per-EVSE clamping happens only inside the battery. A site-level clamp must be added at `update_pilots` time. |
| DR windows, `(start, end]` convention, firm level F, $/kWh penalty on energy overflow, $/kW incentive gated on zero overflow | Absent. No DR event type, no billing for it. |
| FSL billing: `energy(max(net,0)) + demand_rate*max(net) + penalty - incentive` | `analysis.energy_cost`/`demand_charge` bill EV-only power, no netting against building load, no export-earns-nothing clamp. |
| Discharge-aware algorithm inputs (SoC, floor, directional limits, building load, firm level) | `SessionInfo` exposes only energy accounting; SoC/floor invisible; `Interface` has no building-load or DR accessor. |
| Per-slot order: departures, arrivals, assignment, decision, integration | Matches (event precedences 0/10/20). Charger assignment differs: ACN pre-binds `ev.station_id` in the event stream; no waiting queue or `never_connected`. Cross-validation scenarios must be 1 EV : 1 EVSE with no contention. |
| Determinism | Deterministic if ideal `Battery` is used (not `Linear2StageBattery` with noise) and no stochastic events. |

## 3. Change list (file-level anchors, S/M/L effort)

All new code in a new package `acnportal-v2b` (plugin; see section 4), importing acnportal as a
dependency.

1. **`BidirectionalEVSE(EVSE)`** (anchor `acnsim/models/evse.py:232`): `max_discharge_rate` (A);
   `min_rate` returns its negation; `_valid_rate` inherited (pure range check, works signed);
   mirror `_to_dict/_from_dict`. **S**. Caveat: stock preprocessing
   (`algorithms/preprocessing.py`, `sorted_algorithms.py:146` clamps `lb = max(0, ...)`) treats
   `min_pilot` as a deadband; never feed bidirectional EVSEs to stock algorithms.
2. **`BidirectionalBattery(Battery)`** (anchor `acnsim/models/battery.py:12`): `pilot >= 0`
   defers to parent; `pilot < 0` bounded by `max_discharge_power` and `(current - floor)`
   energy, with eta_c/eta_d bookkeeping split between meter side and battery side per SPEC
   section 3; track `_energy_exported` separately so the conservation identity is checkable.
   **M**. Do NOT extend `Linear2StageBattery` for v1.
3. **`V2BEV(EV)`** (anchor `acnsim/models/ev.py:130`): split drawn/exported accumulation; expose
   `soc_kwh`, `floor_kwh`, `max_discharge_power`. **S**.
4. **`V2BChargingNetwork(ChargingNetwork)`** (anchors `charging_network.py:403` `update_pilots`,
   `:126` `active_evs`, `:543` `post_charging_update`): (a) hold `building_load` (kW/period);
   (b) override `update_pilots` to enforce the no-export guard exactly as SPEC section 3
   (nonnegative pilots first, then clamp negative pilots against remaining offsettable draw, in
   fixed station order); (c) override `active_evs` to keep fully-charged-but-connected EVs
   schedulable (stock filter drops them, `:133-137`; a full EV must remain dischargeable);
   (d) use `post_charging_update` to record realized net site load for billing. **M**.
5. **Building load + DR signals**: no core change: `signals["building_load"]` (kW per period)
   and `signals["dr_events"]` (list of `(start, end, F_kw, penalty_rate, incentive_rate,
   baseline_kw)`) in the `Simulator(signals=...)` dict; `Simulator` never introspects it. **S**.
6. **`V2BInterface(Interface)`** (anchor `interface.py:286`; registered via
   `Simulator(interface_type=V2BInterface)`, zero core patching): expose `building_load(start,
   length)`, `active_dr_firm_level(t)` under `(start,end]`, per-session SoC/floor/directional
   limits (`V2BSessionInfo` with signed `min_rates`), and a `discharge_budget(session)` helper
   per SPEC section 4. **M**.
7. **`SeriesTariff`**: duck-types `TimeOfUseTariff` backed by a per-slot price array, so
   openv2b's `grid_prices.csv` maps exactly. **S**.
8. **`v2b_analysis` module**: `net_site_load = building_load + aggregate_power(sim)`; bill per
   SPEC section 5. The `(start,end]` convention and the `*dt` on the penalty are the two places
   parity silently breaks; encode both in one shared helper used by policy and billing. **M**.
9. **Algorithms**: `Uncontrolled`, `EDF`, `LLF`, `EDFV2B`, `LLFV2B` as `BaseAlgorithm`
   subclasses (`algorithms/base_algorithm.py:9`) mirroring SPEC section 4 exactly; emit Amp
   pilots = kW*1000/voltage. **M** each for V2B variants, **S** otherwise.
10. **Scenario bridge**: `openv2b_scenario_to_acnsim(dir)` reading `scenario.json` + CSVs; 1
    EVSE per vehicle, single phase, phase angle 0, voltage chosen so kW-A conversion is exact
    (e.g. 1000 V), one site-cap constraint row of ones. **M**.
11. **DR-boundary recomputes**: inject `RecomputeEvent`s at each DR start/end, or run with
    `max_recompute=1` for parity. **S**.

Not needed (verified): constraint machinery changes, `Simulator.run` loop, event system.

## 4. Fork vs plugin vs upstream-PR recommendation

**Recommendation: PLUGIN package (`acnportal-v2b`), pinned to acnportal==0.3.3, with a short
upstream issue announcing it. Not a fork, not a PR series.**

- **Maintenance reality**: master's last commit is 2023-11-21 (v0.3.3). GitHub API today: 30
  open issues+PRs; PR #85 has activity as recent as 2026-05, PR #124 open since 2026-01, but
  nothing has merged to master in ~2.5 years. An upstream PR series gives no timely, citable
  artifact.
- **The class design explicitly permits subclassing without copying files** at every needed
  point: `interface_type` constructor param, `post_charging_update()` hook, small overridable
  model classes, free-form `signals`, and in-tree contrib precedent.
- **A fork forfeits the credibility payoff**: a plugin lets the claim be "V1G core is
  bit-identical to released acnportal 0.3.3; V2B is an additive layer", which is auditable.
- BSD-3-Clause permits either; the plugin keeps attribution trivial. Optional goodwill: open one
  upstream issue linking the plugin; the EVSE/battery subclasses are PR-able later as contrib.

## 5. Cross-validation experiment matrix

Common setup: `period=15`, ideal batteries (no noise, no 2-stage), eta=1 first, voltage chosen
for exact kW-A conversion, 1:1 EV:EVSE, `max_recompute=1`, single all-ones site-cap constraint.
Compare per-slot aggregate net load, per-session delivered/exported energy, bill components.
Tolerance `max |delta| < 1e-6` after unit conversion for X1-X4; document any slack for X5-X6.

| ID | Scenario | Policy | Compared | Certifies |
|---|---|---|---|---|
| X1 | 1 EV, no building, no DR, charge-only | uncontrolled | per-slot power, delivered kWh | unit/time-convention mapping |
| X2 | 5 EVs staggered, site cap binding | EDF, LLF | aggregate, per-session delivered, target_met | headroom allocation + priority parity |
| X3 | 1 surplus EV, building load, DR mid-dwell | edf-v2b | signed EV power, exported kWh, overflow kWh | discharge dynamics + budget parity |
| X4 | discharge capacity >> building load | llf-v2b | min net load >= 0 in both, clamp locations | no-export guard equivalence |
| X5 | full billing: TOU, demand, one honored + one violated DR window | edf-v2b | itemized bill under (start,end] | billing convention parity (highest value) |
| X6 | surplus regime, staggered departures, 20+ slots | llf-v2b | planned vs realized aggregate drift | receding-horizon information-loss canary |
| X7 | X3 with discharge limits = 0 (negative control) | edf-v2b vs edf | byte-identical within each sim | V2B layer inert when disabled |
| X8 | X3 with eta_c=eta_d=0.92 | edf-v2b | conservation identity in both | efficiency bookkeeping sides agree |

Deliverable per experiment: one runner script reading the same openv2b scenario directory,
running both simulators, emitting a diff table; failures print the first divergent slot.

## 6. Risks

1. `linear=True` feasibility path under-constrains signed schedules (charging_network.py:472-475
   does `abs(M @ s)`, not `abs(M) @ |s|`): V2B code always uses the phasor path; add a guard.
2. Stock-algorithm poisoning: existing preprocessing clamps lower bounds at 0; `V2BInterface`
   refuses registration by non-V2B algorithms unless overridden.
3. `Simulator.peak` / `get_prev_peak` are EV-only aggregate current in Amps (simulator.py:313):
   wrong for demand-charge logic under V2B; deprecate in the plugin, use net-load accessor.
4. Fully-charged EV visibility: without the `active_evs` override, a full EV silently becomes
   non-dischargeable and X3/X4 fail only in surplus regimes (same failure class as
   deficit-vs-surplus regime lessons in the SPEC).
5. Convention drift between the three copies of the DR window definition: one shared helper +
   parity test X5.
6. Dependency rot: acnportal 0.3.3 pins old pandas behavior; pin exact versions, CI on Python
   3.10; `setup.py` declares no `python_requires`.
7. Battery-model asymmetry: openv2b has no CV-tail model; scope the claim to ideal-battery
   parity; present 2-stage runs as an ACN-side sensitivity.
8. Unverified: `contrib/stochastic_network.py` internals, `upper_bound_estimator.py`, the
   `adacharge` companion repo, docs site; maintenance assessed from API metadata only.

**Total effort**: ~7 S + 6 M components, roughly 2-3 focused weeks including the eight parity
experiments; changes 4 (network override) and 8 (billing) are critical path.
