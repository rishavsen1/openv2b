//! Scenario inputs: everything the simulator reads before time starts.
//!
//! A scenario is a directory containing a `scenario.json` manifest plus CSV
//! files for vehicles, chargers, building load, grid prices, and (optionally)
//! demand-response events. All times are expressed in slot indices; all
//! energies in kWh; all powers in kW.

use serde::Deserialize;
use std::path::Path;

/// Simulation-wide parameters, read from `scenario.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Length of one time slot in minutes (e.g. 15).
    pub slot_minutes: f64,
    /// Number of slots in the simulation horizon.
    pub horizon_slots: usize,
    /// Charging efficiency in (0, 1]: grid kWh -> battery kWh.
    #[serde(default = "default_efficiency")]
    pub charge_efficiency: f64,
    /// Discharging efficiency in (0, 1]: battery kWh -> building kWh.
    #[serde(default = "default_efficiency")]
    pub discharge_efficiency: f64,
    /// Site power cap in kW (chargers + building may not exceed this), if any.
    #[serde(default)]
    pub site_cap_kw: Option<f64>,
    /// Demand charge in USD per kW of the billing-period peak net load
    /// (all slots; "facilities-related" component).
    #[serde(default)]
    pub demand_charge_usd_per_kw: f64,
    /// Demand charge in USD per kW of the peak net load over slots whose TOU
    /// class is `peak` ("time-related" component).
    #[serde(default)]
    pub demand_charge_peak_usd_per_kw: f64,
    /// Threshold (kW) seeding the EDF/LLF budget schedulers (OPTIMUS
    /// `historical_max_load`; the converter carries the reference's parquet
    /// lookup). Absent: the reference fallback 0.8 * max building load.
    #[serde(default)]
    pub heuristic_threshold_kw: Option<f64>,
    /// Whether sessions of the same vehicle chain their SoC across sessions
    /// (arrival SoC of a later session = previous departure SoC minus
    /// `depletion_kwh`, clamped). When false, every session uses its own
    /// `soc_arrival_kwh` from the CSV.
    #[serde(default = "default_true")]
    pub persistence: bool,
    /// CSV file names, relative to the scenario directory.
    #[serde(default = "default_vehicles_file")]
    pub vehicles_file: String,
    #[serde(default = "default_chargers_file")]
    pub chargers_file: String,
    #[serde(default = "default_building_file")]
    pub building_file: String,
    #[serde(default = "default_prices_file")]
    pub prices_file: String,
    #[serde(default)]
    pub dr_events_file: Option<String>,
}

fn default_efficiency() -> f64 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_vehicles_file() -> String {
    "vehicles.csv".into()
}
fn default_chargers_file() -> String {
    "chargers.csv".into()
}
fn default_building_file() -> String {
    "building_load.csv".into()
}
fn default_prices_file() -> String {
    "grid_prices.csv".into()
}

/// One charging session request. `vehicle_id` may recur across sessions
/// (persistence); sessions of the same vehicle must not overlap in time.
#[derive(Debug, Clone, Deserialize)]
pub struct Vehicle {
    pub vehicle_id: u32,
    /// First slot the vehicle is plugged in (inclusive).
    pub arrival_slot: usize,
    /// First slot the vehicle is gone (exclusive end of the session).
    pub departure_slot: usize,
    /// Usable battery capacity, kWh.
    pub battery_kwh: f64,
    /// State of charge at arrival, kWh.
    pub soc_arrival_kwh: f64,
    /// State of charge the user wants by departure, kWh.
    pub soc_target_kwh: f64,
    /// Vehicle-side charge power limit, kW.
    pub max_charge_kw: f64,
    /// Vehicle-side discharge power limit, kW (0 disables V2B for this vehicle).
    #[serde(default)]
    pub max_discharge_kw: f64,
    /// Hard SoC floor, kWh: discharge may never take the battery below this.
    #[serde(default)]
    pub min_soc_kwh: f64,
    /// Operating ceiling, kWh (OPTIMUS `max_allowed_soc`): charging may never
    /// take the battery above this. Defaults to the full capacity. The
    /// heuristics' >90% taper anchors to the TRUE capacity (`battery_kwh`),
    /// not to this ceiling, mirroring the reference implementation.
    #[serde(default)]
    pub max_soc_kwh: Option<f64>,
    /// Battery energy consumed between the previous session's departure and
    /// this arrival (driving), kWh. Only meaningful for the second and later
    /// sessions of a vehicle when persistence is on; ignored otherwise.
    #[serde(default)]
    pub depletion_kwh: f64,
}

impl Vehicle {
    /// The effective charging ceiling: `max_soc_kwh` when set, else capacity.
    pub fn ceiling_kwh(&self) -> f64 {
        self.max_soc_kwh.unwrap_or(self.battery_kwh)
    }
}

/// A charging station port.
#[derive(Debug, Clone, Deserialize)]
pub struct Charger {
    pub charger_id: u32,
    /// Port power limit, kW (applies to charge and discharge magnitude).
    pub max_kw: f64,
    /// Whether the port supports discharge (V2B).
    #[serde(default)]
    pub bidirectional: bool,
}

/// Time-of-use class of a slot's price. Affects the time-related demand
/// charge (peak-class slots only) and is visible to policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TouClass {
    Peak,
    #[default]
    OffPeak,
    SuperOffPeak,
}

/// A demand-response event: during the window the building committed to keep
/// its net load at or below `fsl_kw` (firm service level).
///
/// Window convention: the window covers slots `s` with
/// `start_slot < s <= end_slot` (half-open on the left, `(start, end]`).
#[derive(Debug, Clone, Deserialize)]
pub struct DrEvent {
    pub start_slot: usize,
    pub end_slot: usize,
    /// Committed maximum net load during the window, kW.
    pub fsl_kw: f64,
    /// Penalty in USD per kWh of energy above the commitment.
    pub penalty_usd_per_kwh: f64,
    /// Credit in USD per kW of committed reduction, paid if the window is honored.
    #[serde(default)]
    pub incentive_usd_per_kw: f64,
    /// Baseline load the reduction is measured against, kW.
    #[serde(default)]
    pub baseline_kw: f64,
}

impl DrEvent {
    /// Whether `slot` falls inside this event's `(start, end]` window.
    pub fn contains(&self, slot: usize) -> bool {
        slot > self.start_slot && slot <= self.end_slot
    }
}

/// A fully loaded scenario.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub manifest: Manifest,
    pub vehicles: Vec<Vehicle>,
    /// Chargers sorted by `charger_id`.
    pub chargers: Vec<Charger>,
    /// Inflexible building load per slot, kW; length == horizon_slots.
    pub building_load_kw: Vec<f64>,
    /// Grid energy price per slot, USD/kWh; length == horizon_slots.
    pub price_usd_per_kwh: Vec<f64>,
    /// TOU class per slot; length == horizon_slots.
    pub tou_class: Vec<TouClass>,
    pub dr_events: Vec<DrEvent>,
}

#[derive(Debug)]
pub enum ScenarioError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Csv(csv::Error),
    Invalid(String),
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScenarioError::Io(e) => write!(f, "io error: {e}"),
            ScenarioError::Json(e) => write!(f, "manifest error: {e}"),
            ScenarioError::Csv(e) => write!(f, "csv error: {e}"),
            ScenarioError::Invalid(m) => write!(f, "invalid scenario: {m}"),
        }
    }
}

impl std::error::Error for ScenarioError {}

impl From<std::io::Error> for ScenarioError {
    fn from(e: std::io::Error) -> Self {
        ScenarioError::Io(e)
    }
}
impl From<serde_json::Error> for ScenarioError {
    fn from(e: serde_json::Error) -> Self {
        ScenarioError::Json(e)
    }
}
impl From<csv::Error> for ScenarioError {
    fn from(e: csv::Error) -> Self {
        ScenarioError::Csv(e)
    }
}

#[derive(Debug, Deserialize)]
struct SeriesRow {
    slot: usize,
    value: f64,
}

#[derive(Debug, Deserialize)]
struct PriceRow {
    slot: usize,
    value: f64,
    /// Optional TOU class column; a missing column or cell holds the
    /// previous class (initially off-peak).
    #[serde(default)]
    tou: Option<TouClass>,
}

fn read_csv<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>, ScenarioError> {
    let mut reader = csv::Reader::from_path(path)?;
    let mut rows = Vec::new();
    for row in reader.deserialize() {
        rows.push(row?);
    }
    Ok(rows)
}

/// Read a `slot,value` CSV into a dense per-slot vector of length `horizon`.
/// Missing slots keep the previous value (step-and-hold); slots before the
/// first row are 0.
fn read_series(path: &Path, horizon: usize) -> Result<Vec<f64>, ScenarioError> {
    let rows: Vec<SeriesRow> = read_csv(path)?;
    let mut series = vec![0.0; horizon];
    let mut sorted = rows;
    sorted.sort_by_key(|r| r.slot);
    // Duplicate slot rows are ambiguous (which value wins is an
    // implementation accident): reject rather than guess.
    for pair in sorted.windows(2) {
        if pair[0].slot == pair[1].slot {
            return Err(ScenarioError::Invalid(format!(
                "{}: duplicate rows for slot {}",
                path.display(),
                pair[0].slot
            )));
        }
    }
    let mut current = 0.0;
    let mut next_row = sorted.iter().peekable();
    for (slot, out) in series.iter_mut().enumerate() {
        while let Some(row) = next_row.peek() {
            if row.slot <= slot {
                current = row.value;
                next_row.next();
            } else {
                break;
            }
        }
        *out = current;
    }
    Ok(series)
}

/// Read a `slot,value[,tou]` price CSV into dense per-slot price and TOU-class
/// vectors (step-and-hold, like [`read_series`]). A row without a `tou` cell
/// holds the previous class; the initial class is off-peak.
fn read_price_series(
    path: &Path,
    horizon: usize,
) -> Result<(Vec<f64>, Vec<TouClass>), ScenarioError> {
    let mut rows: Vec<PriceRow> = read_csv(path)?;
    rows.sort_by_key(|r| r.slot);
    for pair in rows.windows(2) {
        if pair[0].slot == pair[1].slot {
            return Err(ScenarioError::Invalid(format!(
                "{}: duplicate rows for slot {}",
                path.display(),
                pair[0].slot
            )));
        }
    }
    let mut prices = vec![0.0; horizon];
    let mut classes = vec![TouClass::default(); horizon];
    let mut current_price = 0.0;
    let mut current_class = TouClass::default();
    let mut next_row = rows.iter().peekable();
    for slot in 0..horizon {
        while let Some(row) = next_row.peek() {
            if row.slot <= slot {
                current_price = row.value;
                if let Some(tou) = row.tou {
                    current_class = tou;
                }
                next_row.next();
            } else {
                break;
            }
        }
        prices[slot] = current_price;
        classes[slot] = current_class;
    }
    Ok((prices, classes))
}

impl Scenario {
    /// Load a scenario from a directory containing `scenario.json`.
    pub fn load(dir: &Path) -> Result<Scenario, ScenarioError> {
        let manifest_text = std::fs::read_to_string(dir.join("scenario.json"))?;
        let manifest: Manifest = serde_json::from_str(&manifest_text)?;

        let vehicles: Vec<Vehicle> = read_csv(&dir.join(&manifest.vehicles_file))?;
        let mut chargers: Vec<Charger> = read_csv(&dir.join(&manifest.chargers_file))?;
        chargers.sort_by_key(|c| c.charger_id);
        let building_load_kw =
            read_series(&dir.join(&manifest.building_file), manifest.horizon_slots)?;
        let (price_usd_per_kwh, tou_class) =
            read_price_series(&dir.join(&manifest.prices_file), manifest.horizon_slots)?;
        let dr_events: Vec<DrEvent> = match &manifest.dr_events_file {
            Some(name) => read_csv(&dir.join(name))?,
            None => Vec::new(),
        };

        let scenario = Scenario {
            manifest,
            vehicles,
            chargers,
            building_load_kw,
            price_usd_per_kwh,
            tou_class,
            dr_events,
        };
        scenario.validate()?;
        Ok(scenario)
    }

    /// Check structural invariants of the inputs.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        let m = &self.manifest;
        // Non-finite numbers anywhere would be silently *repaired* downstream
        // (f64::max swallows NaN), understating the bill: reject them here.
        let finite = |x: f64, what: String| -> Result<(), ScenarioError> {
            if x.is_finite() {
                Ok(())
            } else {
                Err(ScenarioError::Invalid(format!(
                    "{what} is not a finite number"
                )))
            }
        };
        finite(m.slot_minutes, "slot_minutes".into())?;
        finite(m.charge_efficiency, "charge_efficiency".into())?;
        finite(m.discharge_efficiency, "discharge_efficiency".into())?;
        finite(
            m.demand_charge_usd_per_kw,
            "demand_charge_usd_per_kw".into(),
        )?;
        finite(
            m.demand_charge_peak_usd_per_kw,
            "demand_charge_peak_usd_per_kw".into(),
        )?;
        if let Some(cap) = m.site_cap_kw {
            finite(cap, "site_cap_kw".into())?;
        }
        for (s, &x) in self.building_load_kw.iter().enumerate() {
            finite(x, format!("building_load at slot {s}"))?;
        }
        for (s, &x) in self.price_usd_per_kwh.iter().enumerate() {
            finite(x, format!("price at slot {s}"))?;
        }
        for v in &self.vehicles {
            for (x, what) in [
                (v.battery_kwh, "battery_kwh"),
                (v.soc_arrival_kwh, "soc_arrival_kwh"),
                (v.soc_target_kwh, "soc_target_kwh"),
                (v.max_charge_kw, "max_charge_kw"),
                (v.max_discharge_kw, "max_discharge_kw"),
                (v.min_soc_kwh, "min_soc_kwh"),
                (v.depletion_kwh, "depletion_kwh"),
            ] {
                finite(x, format!("vehicle {}: {what}", v.vehicle_id))?;
            }
        }
        for c in &self.chargers {
            finite(c.max_kw, format!("charger {}: max_kw", c.charger_id))?;
        }
        for e in &self.dr_events {
            for (x, what) in [
                (e.fsl_kw, "fsl_kw"),
                (e.penalty_usd_per_kwh, "penalty_usd_per_kwh"),
                (e.incentive_usd_per_kw, "incentive_usd_per_kw"),
                (e.baseline_kw, "baseline_kw"),
            ] {
                finite(
                    x,
                    format!("DR event ({}, {}]: {what}", e.start_slot, e.end_slot),
                )?;
            }
        }
        // Charger ids must be unique: they key the trace and per-port checks.
        let mut ids: Vec<u32> = self.chargers.iter().map(|c| c.charger_id).collect();
        ids.sort_unstable();
        for pair in ids.windows(2) {
            if pair[0] == pair[1] {
                return Err(ScenarioError::Invalid(format!(
                    "duplicate charger_id {}",
                    pair[0]
                )));
            }
        }
        if m.slot_minutes <= 0.0 {
            return Err(ScenarioError::Invalid(
                "slot_minutes must be positive".into(),
            ));
        }
        if !(0.0..=1.0).contains(&m.charge_efficiency) || m.charge_efficiency == 0.0 {
            return Err(ScenarioError::Invalid(
                "charge_efficiency must be in (0, 1]".into(),
            ));
        }
        if !(0.0..=1.0).contains(&m.discharge_efficiency) || m.discharge_efficiency == 0.0 {
            return Err(ScenarioError::Invalid(
                "discharge_efficiency must be in (0, 1]".into(),
            ));
        }
        for v in &self.vehicles {
            if v.departure_slot <= v.arrival_slot {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: departure_slot must be after arrival_slot",
                    v.vehicle_id
                )));
            }
            if v.departure_slot > m.horizon_slots {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: departure_slot beyond horizon",
                    v.vehicle_id
                )));
            }
            if v.soc_arrival_kwh < 0.0 || v.soc_arrival_kwh > v.battery_kwh {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: soc_arrival_kwh outside [0, battery_kwh]",
                    v.vehicle_id
                )));
            }
            if v.soc_arrival_kwh < v.min_soc_kwh {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: soc_arrival_kwh below min_soc_kwh (arrives under its own floor)",
                    v.vehicle_id
                )));
            }
            if v.soc_target_kwh < 0.0 || v.soc_target_kwh > v.battery_kwh {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: soc_target_kwh outside [0, battery_kwh]",
                    v.vehicle_id
                )));
            }
            if v.min_soc_kwh < 0.0 || v.min_soc_kwh > v.battery_kwh {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: min_soc_kwh outside [0, battery_kwh]",
                    v.vehicle_id
                )));
            }
            if v.depletion_kwh < 0.0 {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {}: depletion_kwh must be non-negative",
                    v.vehicle_id
                )));
            }
            if let Some(ceiling) = v.max_soc_kwh {
                if !ceiling.is_finite()
                    || ceiling < v.min_soc_kwh
                    || ceiling > v.battery_kwh
                    || v.soc_arrival_kwh > ceiling
                    || v.soc_target_kwh > ceiling
                {
                    return Err(ScenarioError::Invalid(format!(
                        "vehicle {}: max_soc_kwh must lie in [min_soc, battery] and bound arrival/target",
                        v.vehicle_id
                    )));
                }
            }
        }
        // Sessions of the same vehicle must not overlap in time (required for
        // persistence chaining to be well-defined, and physically: one vehicle
        // cannot be parked twice at once).
        let mut sessions: Vec<(u32, usize, usize)> = self
            .vehicles
            .iter()
            .map(|v| (v.vehicle_id, v.arrival_slot, v.departure_slot))
            .collect();
        sessions.sort_unstable();
        for pair in sessions.windows(2) {
            let (id_a, _, dep_a) = pair[0];
            let (id_b, arr_b, _) = pair[1];
            if id_a == id_b && arr_b < dep_a {
                return Err(ScenarioError::Invalid(format!(
                    "vehicle {id_a}: sessions overlap (arrival {arr_b} before departure {dep_a})"
                )));
            }
        }
        for e in &self.dr_events {
            if e.end_slot <= e.start_slot {
                return Err(ScenarioError::Invalid(
                    "DR event with empty (start, end] window".into(),
                ));
            }
            // Covered slots are start+1..=end; all must exist, otherwise the
            // incentive would be paid for a window the simulation never saw.
            if e.end_slot >= self.manifest.horizon_slots {
                return Err(ScenarioError::Invalid(format!(
                    "DR event ({}, {}] extends beyond the horizon ({} slots)",
                    e.start_slot, e.end_slot, self.manifest.horizon_slots
                )));
            }
        }
        // DR events must not overlap: each event settles its own window, so a
        // slot covered twice would be penalized (and incentivized) twice.
        let mut windows: Vec<(usize, usize)> = self
            .dr_events
            .iter()
            .map(|e| (e.start_slot, e.end_slot))
            .collect();
        windows.sort_unstable();
        for pair in windows.windows(2) {
            // (s1, e1] and (s2, e2] with s1 <= s2 are disjoint iff s2 >= e1.
            if pair[1].0 < pair[0].1 {
                return Err(ScenarioError::Invalid(format!(
                    "DR events overlap: ({}, {}] and ({}, {}]",
                    pair[0].0, pair[0].1, pair[1].0, pair[1].1
                )));
            }
        }
        // Rows sharing a vehicle_id are sessions of one physical vehicle:
        // battery, floor, and power limits must agree or the persistence
        // chain clamps against contradictory bounds.
        let mut by_id: std::collections::HashMap<u32, &Vehicle> = std::collections::HashMap::new();
        for v in &self.vehicles {
            if let Some(first) = by_id.get(&v.vehicle_id) {
                if v.battery_kwh != first.battery_kwh
                    || v.min_soc_kwh != first.min_soc_kwh
                    || v.max_soc_kwh != first.max_soc_kwh
                    || v.max_charge_kw != first.max_charge_kw
                    || v.max_discharge_kw != first.max_discharge_kw
                {
                    return Err(ScenarioError::Invalid(format!(
                        "vehicle {}: sessions disagree on battery/floor/power limits",
                        v.vehicle_id
                    )));
                }
            } else {
                by_id.insert(v.vehicle_id, v);
            }
        }
        Ok(())
    }
}
