# Scenario input format

A scenario is a directory containing `scenario.json` plus CSV files. See `examples/one_day/`.

## scenario.json

| key | type | default | meaning |
|---|---|---|---|
| `slot_minutes` | number | required | slot length in minutes |
| `horizon_slots` | integer | required | number of slots simulated |
| `charge_efficiency` | number | 1.0 | grid kWh -> battery kWh, in (0, 1] |
| `discharge_efficiency` | number | 1.0 | battery kWh -> building kWh, in (0, 1] |
| `site_cap_kw` | number | none | site power cap, engine-enforced on EV charging |
| `demand_charge_usd_per_kw` | number | 0 | facilities rate on the all-slots peak |
| `demand_charge_peak_usd_per_kw` | number | 0 | time-related rate on the peak-TOU-class peak |
| `persistence` | bool | true | chain SoC across sessions of the same vehicle |
| `vehicles_file` | string | `vehicles.csv` | |
| `chargers_file` | string | `chargers.csv` | |
| `building_file` | string | `building_load.csv` | |
| `prices_file` | string | `grid_prices.csv` | |
| `dr_events_file` | string | none | omit for no demand response |

## vehicles.csv

One row per charging session. Columns: `vehicle_id` (integer), `arrival_slot` (inclusive),
`departure_slot` (exclusive), `battery_kwh`, `soc_arrival_kwh`, `soc_target_kwh`,
`max_charge_kw`, `max_discharge_kw` (optional, 0 = no V2B), `min_soc_kwh` (optional, 0),
`depletion_kwh` (optional, 0: battery energy consumed since the previous session's departure).
Rows sharing a `vehicle_id` are sessions of one vehicle: they must not overlap in time and must
agree on `battery_kwh`, `min_soc_kwh`, and both power limits. Under persistence, the
`soc_arrival_kwh` of the second and later sessions is ignored (the chain computes it); keep a
physically sensible value there anyway so `persistence: false` runs remain meaningful.

## chargers.csv

`charger_id` (integer), `max_kw` (per-direction port limit), `bidirectional` (true/false).

## building_load.csv and grid_prices.csv

Sparse step functions: `slot,value` rows. The value holds until the next row's slot
(step-and-hold); slots before the first row are 0. Units: kW for load, USD/kWh for prices.
`grid_prices.csv` accepts an optional third column `tou` (`peak`, `off-peak`,
`super-off-peak`), also step-and-hold; the initial class is off-peak.

## dr_events.csv

`start_slot`, `end_slot` (window covers slots `start < s <= end`), `fsl_kw` (committed firm
service level), `penalty_usd_per_kwh`, `incentive_usd_per_kw` (optional), `baseline_kw`
(optional; the level the incentive reduction is measured against). Windows must lie entirely
inside the horizon and must not overlap each other; back-to-back windows are fine.
