//! Faithful ports of the OPTIMUS heuristic decision policies.
//!
//! Every rule here is transcribed from the reference implementation (see
//! docs/OPTIMUS_PORT.md for the line-anchored spec); nothing is redesigned.
//! Known deliberate divergences (documented, per project rulings):
//! - openv2b's engine keeps strict physics (no grid export, hard SoC clamps);
//!   the reference validates via exceptions and does not clamp.
//! - Sorting is stable with an explicit `vehicle_id` tie-break; the reference
//!   uses unstable quicksort whose tie order is unspecified.
//! - The reference's POLICY_3 is omitted: its discharge leg calls a method
//!   that does not exist, so it has never successfully run there.
//!
//! Port glossary (reference term -> here):
//! - SoC percent -> computed as `soc_kwh / battery_kwh * 100` (battery_kwh is
//!   the TRUE capacity; the operating ceiling is `max_soc_kwh`).
//! - `historical_max_load` -> the threshold; seeded from the manifest's
//!   `heuristic_threshold_kw` (the converter carries the reference's parquet
//!   lookup value) or the reference's fallback `0.8 * max(building series)`.

use super::Policy;
use crate::scenario::{TouClass, Vehicle};
use crate::state::{Observation, SessionView, Setpoint};
use std::cell::RefCell;

/// The reference's `time_before_heuristics`: force-charge window, seconds.
/// Not configurable there either (no ini key exists).
const TIME_BEFORE_HEURISTICS_SEC: f64 = 3600.0;

/// Never charges or discharges anything. The building-only baseline for
/// EV-vs-building cost attribution. (openv2b-native; not an OPTIMUS policy.)
pub struct Idle;

impl Policy for Idle {
    fn name(&self) -> &'static str {
        "idle"
    }
    fn decide(&self, _obs: &Observation) -> Vec<Setpoint> {
        Vec::new()
    }
}

/// Charge every vehicle toward its target at maximum feasible power.
/// (openv2b-native baseline; not an OPTIMUS policy.)
pub struct Uncontrolled;

impl Policy for Uncontrolled {
    fn name(&self) -> &'static str {
        "uncontrolled"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| {
                let need = (s.vehicle.soc_target_kwh - s.soc_kwh).max(0.0);
                let kw = if need > 0.0 {
                    crate::kwh_to_kw(need / obs.charge_efficiency, obs.slot_minutes)
                        .min(s.max_charge_kw)
                } else {
                    0.0
                };
                Setpoint {
                    session_index: s.index,
                    power_kw: kw,
                }
            })
            .collect()
    }
}

// ------------------------------------------------------------------ shared

/// The reference's `EnergyProperties.get_rate`: the >90% charge taper and the
/// hard discharge floor. Percentages are of TRUE capacity; the taper knee
/// (90) and zero point (100) are hardcoded there and independent of the
/// vehicle's own ceiling. Comparisons are exact (no tolerance).
fn get_rate(v: &Vehicle, soc_kwh: f64, rate_kw: f64) -> f64 {
    let soc = soc_kwh / v.battery_kwh * 100.0;
    let max_soc = v.ceiling_kwh() / v.battery_kwh * 100.0;
    let min_soc = v.min_soc_kwh / v.battery_kwh * 100.0;
    if rate_kw > 0.0 {
        if soc <= max_soc {
            if soc <= 90.0 {
                rate_kw
            } else {
                -rate_kw / 10.0 * (soc - 90.0) + rate_kw
            }
        } else {
            0.0
        }
    } else if rate_kw < 0.0 {
        if soc >= min_soc {
            rate_kw
        } else {
            0.0
        }
    } else {
        0.0
    }
}

/// The reference's `np.isclose(a, b, atol=0.1)` on SoC percentages:
/// `|a - b| <= 0.1 + 1e-5 * |b|` (numpy's default rtol against the second
/// argument). Used by POLICY_0/1/2 eligibility; NOT by EDF/LLF, whose
/// eligibility is strict.
fn isclose_pct(a: f64, b: f64) -> bool {
    (a - b).abs() <= 0.1 + 1e-5 * b.abs()
}

fn soc_pct(s: &SessionView) -> f64 {
    s.soc_kwh / s.vehicle.battery_kwh * 100.0
}

fn under_req_toleranced(s: &SessionView) -> bool {
    let (soc, req) = (
        soc_pct(s),
        s.vehicle.soc_target_kwh / s.vehicle.battery_kwh * 100.0,
    );
    soc < req && !isclose_pct(soc, req)
}

fn under_max_toleranced(s: &SessionView) -> bool {
    let (soc, max) = (
        soc_pct(s),
        s.vehicle.ceiling_kwh() / s.vehicle.battery_kwh * 100.0,
    );
    soc < max && !isclose_pct(soc, max)
}

fn over_req_toleranced(s: &SessionView) -> bool {
    let (soc, req) = (
        soc_pct(s),
        s.vehicle.soc_target_kwh / s.vehicle.battery_kwh * 100.0,
    );
    soc > req && !isclose_pct(soc, req)
}

fn time_left_sec(s: &SessionView, obs: &Observation) -> f64 {
    (s.vehicle.departure_slot - obs.slot) as f64 * obs.slot_minutes * 60.0
}

// ---------------------------------------------------------------- POLICY_0

/// Reference POLICY_0: charge each below-target car at exactly the minimum
/// constant rate that reaches the target by departure; never discharge.
pub struct Policy0;

impl Policy for Policy0 {
    fn name(&self) -> &'static str {
        "policy-0"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| {
                let mut rate = 0.0;
                if under_req_toleranced(s) {
                    let need_kwh = s.vehicle.soc_target_kwh - s.soc_kwh;
                    let hours = time_left_sec(s, obs) / 3600.0;
                    rate = need_kwh / hours;
                    if rate > s.max_charge_kw {
                        rate = s.max_charge_kw;
                    }
                    if rate < 0.0 {
                        rate = 0.0;
                    }
                    rate = get_rate(s.vehicle, s.soc_kwh, rate);
                }
                Setpoint {
                    session_index: s.index,
                    power_kw: rate,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------- POLICY_1

/// Reference POLICY_1: off-peak/super-off-peak, charge below-ceiling cars at
/// the charger's full rate; at peak, discharge above-target cars at the
/// charger's full negative rate. The two passes overlap and the second wins,
/// so off-peak a car already above its target is STOPPED, not charged (a
/// documented reference behavior, not an accident).
pub struct Policy1;

impl Policy for Policy1 {
    fn name(&self) -> &'static str {
        "policy-1"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut out = Vec::new();
        for s in obs.sessions.iter().filter(|s| under_max_toleranced(s)) {
            let rate = match obs.tou {
                TouClass::OffPeak | TouClass::SuperOffPeak => {
                    get_rate(s.vehicle, s.soc_kwh, s.max_charge_kw)
                }
                TouClass::Peak => 0.0,
            };
            out.push(Setpoint {
                session_index: s.index,
                power_kw: rate,
            });
        }
        for s in obs.sessions.iter().filter(|s| over_req_toleranced(s)) {
            let rate = match obs.tou {
                TouClass::Peak => get_rate(s.vehicle, s.soc_kwh, -s.max_discharge_kw),
                _ => 0.0,
            };
            // Later setpoints override earlier ones in the engine, exactly
            // like the reference's second loop overwriting the action slot.
            out.push(Setpoint {
                session_index: s.index,
                power_kw: rate,
            });
        }
        out
    }
}

// ---------------------------------------------------------------- POLICY_2

/// Reference POLICY_2: charge below-ceiling cars at full rate ONLY when the
/// TOU class is exactly off-peak (super-off-peak charges nothing there; a
/// reference behavior a port must not "clean up"). Never discharges.
pub struct Policy2;

impl Policy for Policy2 {
    fn name(&self) -> &'static str {
        "policy-2"
    }
    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .filter(|s| under_max_toleranced(s))
            .map(|s| {
                let rate = if obs.tou == TouClass::OffPeak {
                    get_rate(s.vehicle, s.soc_kwh, s.max_charge_kw)
                } else {
                    0.0
                };
                Setpoint {
                    session_index: s.index,
                    power_kw: rate,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------- EDF / LLF (threshold)

enum SortKey {
    /// Reference EDF: descending "deadline pressure" =
    /// `100 * power_required_kwh * max_charge / ((L_hist - L_bldg) * time_left_sec)`.
    DeadlinePressure,
    /// Reference LLF: ascending `time_left` (despite the name, no laxity is
    /// computed in the reference; this IS its algorithm).
    TimeLeft,
}

/// The threshold-budget scheduler behind both `EARLY_DEADLINE_FIRST_BID` and
/// `LEAST_LAXITY_FIRST_BID`. Stateful: the threshold ratchets monotonically
/// upward across the episode, exactly like the reference instance attribute.
pub struct ThresholdScheduler {
    key: SortKey,
    name: &'static str,
    /// `historical_max_load`, lazily seeded on first decide.
    threshold_kw: RefCell<Option<f64>>,
}

impl ThresholdScheduler {
    pub fn edf() -> Self {
        ThresholdScheduler {
            key: SortKey::DeadlinePressure,
            name: "edf",
            threshold_kw: RefCell::new(None),
        }
    }
    pub fn llf() -> Self {
        ThresholdScheduler {
            key: SortKey::TimeLeft,
            name: "llf",
            threshold_kw: RefCell::new(None),
        }
    }
}

struct Row {
    session_index: usize,
    vehicle_id: u32,
    min_rate_needed_kw: f64,
    time_left_sec: f64,
    max_charge_kw: f64,
    max_discharge_kw: f64,
    key: f64,
}

impl Policy for ThresholdScheduler {
    fn name(&self) -> &'static str {
        self.name
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut threshold = self.threshold_kw.borrow_mut();
        let l_hist = threshold.get_or_insert_with(|| {
            obs.heuristic_threshold_kw.unwrap_or_else(|| {
                // Reference fallback: 0.8 * max of the episode's building load.
                0.8 * obs
                    .building_series
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max)
            })
        });

        // Eligibility: STRICT inequalities (no tolerance), unlike POLICY_0-2.
        // Peak: below target. Off-peak and super-off-peak: below ceiling.
        let peak = obs.tou == TouClass::Peak;
        let mut rows: Vec<Row> = obs
            .sessions
            .iter()
            .filter(|s| {
                if peak {
                    s.soc_kwh < s.vehicle.soc_target_kwh
                } else {
                    s.soc_kwh < s.vehicle.ceiling_kwh()
                }
            })
            .map(|s| {
                // power_required is SIGNED: negative when above target, which
                // is the reference's implicit off-peak discharge channel.
                let need_kwh = s.vehicle.soc_target_kwh - s.soc_kwh;
                let tl = time_left_sec(s, obs);
                let mut min_rate = need_kwh / (tl / 3600.0);
                if min_rate.is_infinite() {
                    min_rate = 0.0; // the reference coerces +/-inf (not NaN)
                }
                let key = match self.key {
                    SortKey::DeadlinePressure => {
                        100.0 * need_kwh * s.max_charge_kw / ((*l_hist - obs.building_load_kw) * tl)
                    }
                    SortKey::TimeLeft => tl,
                };
                Row {
                    session_index: s.index,
                    vehicle_id: s.vehicle.vehicle_id,
                    min_rate_needed_kw: min_rate,
                    time_left_sec: tl,
                    max_charge_kw: s.max_charge_kw,
                    max_discharge_kw: s.max_discharge_kw,
                    key,
                }
            })
            .collect();

        // Stable sort; NaN keys last; explicit vehicle_id tie-break (the
        // documented divergence from the reference's unstable quicksort).
        let descending = matches!(self.key, SortKey::DeadlinePressure);
        rows.sort_by(|a, b| {
            let ord = match (a.key.is_nan(), b.key.is_nan()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater, // NaN last
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => {
                    let o = a.key.partial_cmp(&b.key).expect("both finite-ish");
                    if descending {
                        o.reverse()
                    } else {
                        o
                    }
                }
            };
            ord.then(a.vehicle_id.cmp(&b.vehicle_id))
        });

        let view = |idx: usize| &obs.sessions[idx];
        let mut setpoints: Vec<Setpoint> = Vec::new();
        let mut served: Vec<u32> = Vec::new();

        // Budget walk. `capacity` and `used_power` double-track the budget,
        // including the reference's exact clip arithmetic (the guard uses the
        // already-decremented capacity; the clip is against remaining
        // capacity). Copied verbatim; do not "fix".
        let mut capacity = *l_hist - obs.building_load_kw;
        let mut used_power = 0.0;
        for row in &rows {
            if capacity <= 0.0 {
                break;
            }
            let s = view(row.session_index);
            let mut rate = row.min_rate_needed_kw;
            let original = rate;
            if used_power + rate > capacity {
                rate = rate.min(capacity);
            }
            rate = rate.min(row.max_charge_kw);
            rate = get_rate(s.vehicle, s.soc_kwh, rate);
            setpoints.push(Setpoint {
                session_index: row.session_index,
                power_kw: rate,
            });
            if rate >= original {
                served.push(row.vehicle_id);
            }
            used_power += rate;
            capacity -= rate;
        }

        // Force-charge fallback: any eligible car within the window that was
        // not fully served gets its full needed rate OUTSIDE the budget
        // (positive needs capped at the charger max; negative needs floored
        // at the discharge limit: this is the reference's metered discharge
        // channel). The taper still applies. `capacity` is NOT decremented.
        for row in &rows {
            if row.time_left_sec < TIME_BEFORE_HEURISTICS_SEC && !served.contains(&row.vehicle_id) {
                let s = view(row.session_index);
                let rate = if row.min_rate_needed_kw > 0.0 {
                    row.min_rate_needed_kw.min(row.max_charge_kw)
                } else {
                    row.min_rate_needed_kw.max(-row.max_discharge_kw)
                };
                let rate = get_rate(s.vehicle, s.soc_kwh, rate);
                // Later setpoint wins in the engine == the reference's
                // action-slot overwrite.
                setpoints.push(Setpoint {
                    session_index: row.session_index,
                    power_kw: rate,
                });
                used_power += rate;
            }
        }

        // Adaptive ratchet: monotone, instance-persistent.
        if obs.building_load_kw + used_power > *l_hist {
            *l_hist = obs.building_load_kw + used_power;
        }

        setpoints
    }
}
