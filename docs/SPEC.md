# openv2b behavioral specification

This is the normative description of what the simulator computes. Code implements this document;
when code and spec disagree, one of them has a bug and the discrepancy must be resolved explicitly.
All quantities carry SI-adjacent units: energy in kWh, power in kW, money in USD, time in slot
indices. Conversion: `kWh per slot = kW * slot_minutes / 60`.

## 1. Time model

- Time is discretized into uniform slots of `slot_minutes` (canonically 15). The horizon is
  `horizon_slots` slots; one simulation covers one billing period.
- Within a slot, all powers are constant. The per-slot processing order is fixed:
  1. **Departures**: every vehicle with `departure_slot == s` is disconnected; its charger frees.
  2. **Arrivals**: every vehicle with `arrival_slot == s` joins the waiting queue, ordered by
     `(arrival_slot, vehicle_id)`.
  3. **Charger assignment**: waiting vehicles claim free chargers in queue order, using the
     capability-aware port preference described in section 4. A vehicle that finds no charger
     waits; if it departs while still waiting it is reported as `never_connected`.
  4. **Decision**: the policy sees the observation (section 4) and returns one signed power
     setpoint per connected session.
  5. **Integration**: the engine clamps each setpoint to physical limits and integrates energy
     for the slot (section 3).

  Departures-before-arrivals is load-bearing: it frees chargers for same-slot arrivals.
- A session occupies slots `arrival_slot <= s < departure_slot` (half-open on the right).
- Determinism is a hard requirement: identical inputs produce byte-identical outputs. No wall
  clock, no unseeded randomness anywhere in the simulation path.

## 2. Entities

**Vehicle / session**: identity, arrival and departure slots, usable battery capacity (kWh),
arrival SoC (kWh), departure target SoC (kWh), hard SoC floor (kWh), vehicle-side charge and
discharge power limits (kW). A discharge limit of 0 disables V2B for that vehicle.

**Charger**: port power limit (kW, applies to both directions) and a bidirectional flag. The
effective limit for a session is `min(vehicle limit, port limit)` per direction; discharge
additionally requires the port to be bidirectional.

**Building**: an inflexible base load series (kW per slot).

**Grid**: an energy price series (USD/kWh per slot) and a demand charge rate (USD/kW).

**DR event**: a window with a committed firm service level `F` (kW), a penalty rate (USD/kWh),
an incentive rate (USD/kW), and a baseline (kW).

## 3. Dynamics

Let `dt = slot_minutes / 60` hours, `eta_c` = charge efficiency, `eta_d` = discharge efficiency
(both default 1, lossless).

- **Charging** at grid-side power `p >= 0` for one slot: the meter records `p * dt` kWh drawn;
  the battery gains `p * dt * eta_c` kWh. Clamped so SoC never exceeds capacity.
- **Discharging** at building-side power `p >= 0`: the building's load is offset by `p * dt` kWh;
  the battery loses `p * dt / eta_d` kWh. Clamped so SoC never falls below the vehicle's floor.
- **No export**: aggregate discharge in a slot may offset the site's total draw (building load
  plus fleet charging) but never exceed it. Net site load is always >= 0. The engine enforces
  this by applying charge setpoints first, then discharge setpoints against the remaining
  offsettable draw, in the policy's emission order.
- **Site cap**: when `site_cap_kw` is set, the engine clamps aggregate EV charging so the site
  never exceeds `max(building load, cap)` (the building's own load is not curtailable). This is
  engine-enforced for every policy, not advisory.
- The engine, not the policy, is the authority on feasibility: infeasible requests are clamped,
  never propagated. Non-finite setpoints (NaN, +/-inf) are discarded before deduplication, and
  at most one *finite* setpoint per session is honored per slot (the last emitted wins), so a
  session can never charge and discharge simultaneously. Consequently every invariant in
  section 7 holds for arbitrary policies, including adversarial ones.
- **Arbitration order**: when a shared resource binds (site-cap charge headroom, no-export
  discharge headroom), the engine rations it in the POLICY'S EMISSION ORDER: the order
  setpoints were returned is the priority order, with an overridden setpoint keeping its latest
  emission position. Sessions are presented to policies in a canonical order
  (arrival slot, vehicle id), never CSV row order, so identical scenarios with permuted input
  rows are indistinguishable.
- **Persistence**: a vehicle identity recurring across sessions carries
  `SoC(arrival, k+1) = clamp(SoC(departure, k) - depletion_kwh, floor, capacity)`, resolved at
  arrival time (departures process first, so same-slot handoffs chain correctly). Unserved
  sessions propagate their unchanged SoC. If the declared depletion exceeds what the battery
  held, the clamp's manufactured energy is reported per session as `chain_clamped_kwh`, never
  silent. The manifest flag `persistence: false` restores independent sessions (each row's CSV
  arrival SoC is used as-is). **Banking** is charging above the departure target so the surplus
  survives to the next session; each session reports `banked_kwh` and `missing_kwh`.

## 4. Policy interface

A policy is a deterministic pure function of the observation:

- observation: slot index, slot length, this slot's building load and price, the full price
  series (day-ahead prices are public information), site cap, the active DR firm level (minimum
  over covering events) if any, both efficiencies, and one view per connected session (static
  request data + live SoC + effective directional power limits);
- decision: a list of `(session, signed power kW)` setpoints; positive charges, negative
  discharges. Unlisted sessions default to 0.

Built-in policies:

- **uncontrolled**: every session charges at min(need expressed as grid power, its limit) until
  the target is reached. The reference "dumb charging" baseline.
- **edf / llf**: priority scheduling. EDF orders by departure slot; LLF by laxity = slots
  remaining minus slots needed at full power (recomputed every slot). Both allocate
  charge-toward-target power in priority order against the slot's headroom
  (min(site cap, DR firm level) minus building load). **Force-charge fallback**: once a
  session's laxity reaches zero (the target is only just reachable at full power), the economic
  headroom yields to the service guarantee: the session charges at its full target rate even
  inside a DR window, eating the window penalty. Physical limits (site cap, no-export) still
  apply. Without this, a DR window whose firm level sits below the building load would starve a
  trivially feasible session.
- **edf-v2b / llf-v2b**: same, plus **banking** and a discharge overlay. Banking: outside
  peak-price TOU slots the charge goal is the battery capacity rather than the departure
  target, so surplus accumulates for later windows (without banking, a persistence-chained
  donor whose sessions only charge to target runs permanently dry after its first discharge,
  because chained arrivals can never exceed the previous departure). Discharge overlay during DR
  windows: if net load exceeds the firm level, sessions are visited in reverse priority order,
  first cancelling planned charging (never below a force-charged session's needs) and then
  discharging — but only within each session's *discharge budget*: the battery-side surplus
  above `max(departure target, SoC floor)`, converted to building-side power via the discharge
  efficiency. Surplus-only discharge is deliberately conservative: a "borrow now, recharge
  later" budget requires knowing future charging headroom (which a DR window binds to zero) and
  was demonstrated to sacrifice departure targets when the window abuts departure. This budget
  guarantees V2B never causes a target miss, whatever follows (tested, including under
  asymmetric efficiencies).
- **idle**: never charges or discharges; the building-only baseline for EV-vs-building cost
  attribution.

**Charger assignment** is capability-aware: V2B-capable vehicles (discharge limit > 0) prefer
the lowest free bidirectional port, others prefer unidirectional ports, with fallback to any
free port; queue order is (arrival slot, vehicle id). This prevents a discharge resource being
stranded on a unidirectional port while a bidirectional port sits free.

## 5. Billing

`total = energy + demand_facilities + demand_peak_tou + dr_penalty - dr_incentive`

- **Energy**: sum over slots of `max(net_kw, 0) * dt * price`. Exports earn nothing.
- **Demand**, two itemized components: `demand_charge_usd_per_kw * (max net over ALL slots)`
  (facilities-related) plus `demand_charge_peak_usd_per_kw * (max net over slots whose TOU class
  is peak)` (time-related). Each price row may carry a `tou` class (peak / off-peak /
  super-off-peak, step-and-hold like the price itself). The peak-TOU peak is by construction
  <= the all-slots peak. Ratchet and minimum-demand clauses are a documented gap.
- **DR window convention**: an event `(start, end)` covers slots `start < s <= end` — half-open
  on the *left*. The identical window definition must be used by billing, by policies, and by any
  future optimization formulation; changing one side alone silently breaks planner/bill parity.
- **DR penalty**: `penalty_rate [$/kWh] * sum over covered slots of max(net_kw - F, 0) * dt`.
  The `* dt` matters: the penalty is on *energy* above the commitment, so a $/kWh rate applies to
  a kWh quantity.
- **DR incentive**: `incentive_rate [$/kW] * max(baseline - F, 0)`, paid only if the window was
  honored (zero overflow). A building that is not enrolled (no DR events in the scenario) simply
  has no DR terms — there is no implicit default commitment.

## 6. Inputs and outputs

Input: a directory with `scenario.json` (slot length, horizon, efficiencies, tariff rates,
persistence flag, file names) plus `vehicles.csv`, `chargers.csv`, `building_load.csv`,
`grid_prices.csv`, and optional `dr_events.csv`. Series files are sparse `slot,value[,tou]` step
functions densified by step-and-hold. All inputs are validated on load; invalid scenarios are
rejected, never repaired. Validation rules: every numeric field and series value must be finite
(NaN/inf would otherwise be silently swallowed by max() into an under-stated bill); sessions
non-empty and within the horizon; arrival SoC within [floor, capacity]; target within
[0, capacity]; floor within [0, capacity]; efficiencies in (0,1]; depletion non-negative;
sessions of one vehicle must not overlap and must agree on battery/floor/power limits; charger
ids unique; series files must not repeat a slot index; DR windows non-empty, entirely inside
the horizon (`end_slot < horizon`), and mutually disjoint (each slot settles at most once). A
DR window can never cover slot 0 (a `(start, end]` representation artifact; the earliest
coverable slot is 1).

Output: `slots.csv` (per-slot power flows, price, TOU class), `sessions.csv` (per-session
outcome including `target_met`, `missing_kwh`, `banked_kwh`, `chain_clamped_kwh`,
`never_connected`), `trace.csv` (per-slot per-session applied power and end-of-slot SoC, for
external verification of charger exclusivity and exact trajectories), `summary.json` (itemized
bill + service counts + fleet energy totals).

## 7. Invariants (each has a test; the randomized property sweep asserts the
physical ones over 200 seeded scenarios x all policies with coverage counters)

1. **Energy conservation**: per session, `SoC_dep - SoC_arr = eta_c * drawn - exported / eta_d`
   (`SoC_arr` is the *effective* arrival SoC, i.e. the chained value under persistence).
2. **SoC bounds**: SoC stays within `[floor, capacity]` for every policy.
3. **No export**: net site load is never negative, even under adversarial discharge capability.
4. **Power caps**: per-port and aggregate charge/discharge never exceed physical limits; when a
   site cap is set, net load never exceeds `max(building, cap)` under ANY policy.
5. **Determinism**: two runs serialize byte-identically (also verified as two separate OS
   processes with SHA-256 in the verification driver), and outcomes are invariant to CSV row
   permutation.
6. **Bill identity**: the total equals the sum of its itemized parts; the reported DR overflow
   equals the windowed positive part times slot-hours, under the `(start, end]` convention
   (anchored by a hand-written covered-slot table independent of the code); incentives are paid
   only for honored windows with at least one simulated covered slot.
7. **Service**: trivially feasible targets are met by every built-in policy; V2B discharge never
   causes a target miss (including when a DR window abuts departure, and under asymmetric
   efficiencies); unserved sessions are reported, never dropped, and propagate their SoC through
   the persistence chain.
8. **V2B effectiveness**: with surplus energy available, the V2B variant strictly reduces DR
   overflow (and the bill) relative to its charge-only twin.
9. **Adversarial policies**: NaN/inf/huge/out-of-range/duplicate setpoints cannot corrupt any of
   the above; slot records keep non-negative charge and discharge columns.
10. **Site energy balance**: `sum(net * dt) = building energy + total drawn - total exported`
    ties `slots.csv` to `sessions.csv`; the per-slot trace reconciles exactly with both.
11. **Persistence chain identity**: `SoC_arr(k+1) = clamp(SoC_dep(k) - depletion, floor,
    capacity)`, with the clamp's manufactured energy reported (`chain_clamped_kwh`), banked
    surplus reducing the next session's grid draw one-for-one (lossless case), and
    `persistence: false` restoring independent sessions.

## 8. Optimization-based policies (v0.3-alpha, implemented)

A receding-horizon MPC (`policy::mpc`) over the solver-agnostic `milp` layer (see
docs/SOLVER_DESIGN.md): a `MilpBackend` trait with an in-process HiGHS backend (cargo feature
`solver-highs`, the open default) and a dependency-free LP-file + CLI backend that drives any
solver (verified bill-identical against a local CPLEX 22.1). The formulation is a pure LP:
per-session per-slot split charge/discharge energy variables with SoC recursion under
efficiencies; a reachability terminal condition with a high-penalty shortfall slack; aggregate
net-load variables with no-export and site-cap bounds; peak variables for both demand
components; DR overflow variables priced at each event's penalty rate; a battery-degradation
cost on discharge. Honest information set: only currently-connected sessions plus public series
(no future arrivals). The engine still clamps every setpoint, so a wrong solve can cost money
but never break physics (tested).

Still planned: an in-process Gurobi backend (`grb` crate), committed-firm-level (FSL)
optimization against a counterfactual no-DR baseline, and MPC-vs-oracle parity suites. Those
parity suites must cover both the *deficit* regime (arrive low, need high, no V2B) and the
*surplus* regime (arrive high, need low, V2B-heavy) with staggered departures — the surplus
regime is where receding-horizon information-loss bugs hide — plus a drift canary: under
perfect foresight the planned peak must never jump upward between successive re-solves.
Determinism requires pinning solver threads and seed (both backends do).
