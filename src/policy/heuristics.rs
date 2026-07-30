//! Built-in heuristic policies: Uncontrolled, Earliest-Deadline-First, and
//! Least-Laxity-First, each optionally with a V2B peak-shaving overlay that
//! discharges surplus battery energy during demand-response windows.

use super::Policy;
use crate::state::{Observation, SessionView, Setpoint};

/// Charge every vehicle at its maximum feasible power until it reaches its
/// target, ignoring prices, site caps other than hard limits, and DR windows.
pub struct Uncontrolled;

impl Policy for Uncontrolled {
    fn name(&self) -> &'static str {
        "uncontrolled"
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        obs.sessions
            .iter()
            .map(|s| Setpoint {
                session_index: s.index,
                power_kw: charge_power_toward_target(s, obs),
            })
            .collect()
    }
}

/// Priority scheduling by earliest departure slot; ties broken by vehicle id.
pub struct EarliestDeadlineFirst {
    pub v2b: bool,
}

impl Policy for EarliestDeadlineFirst {
    fn name(&self) -> &'static str {
        if self.v2b {
            "edf-v2b"
        } else {
            "edf"
        }
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut order: Vec<usize> = (0..obs.sessions.len()).collect();
        order.sort_by_key(|&i| {
            let s = &obs.sessions[i];
            (s.vehicle.departure_slot, s.vehicle.vehicle_id)
        });
        prioritized_allocation(obs, &order, self.v2b)
    }
}

/// Priority scheduling by least laxity (slack before the target becomes
/// unreachable); ties broken by vehicle id. Laxity is recomputed every slot,
/// so this is the dynamic variant of EDF.
pub struct LeastLaxityFirst {
    pub v2b: bool,
}

impl Policy for LeastLaxityFirst {
    fn name(&self) -> &'static str {
        if self.v2b {
            "llf-v2b"
        } else {
            "llf"
        }
    }

    fn decide(&self, obs: &Observation) -> Vec<Setpoint> {
        let mut order: Vec<usize> = (0..obs.sessions.len()).collect();
        // Sort by laxity ascending; f64 keys via total_cmp for determinism.
        order.sort_by(|&a, &b| {
            let la =
                obs.sessions[a].laxity_slots(obs.slot, obs.slot_minutes, obs.charge_efficiency);
            let lb =
                obs.sessions[b].laxity_slots(obs.slot, obs.slot_minutes, obs.charge_efficiency);
            la.total_cmp(&lb).then(
                obs.sessions[a]
                    .vehicle
                    .vehicle_id
                    .cmp(&obs.sessions[b].vehicle.vehicle_id),
            )
        });
        prioritized_allocation(obs, &order, self.v2b)
    }
}

/// Battery-side energy this session still wants this slot: the deficit to
/// its departure target, or (for V2B policies outside peak-price slots) the
/// deficit to full capacity — charging above the target *banks* energy that
/// the discharge overlay can later return during a DR window.
fn desired_kwh(s: &SessionView, obs: &Observation, v2b: bool) -> f64 {
    let bank = v2b && obs.tou != crate::scenario::TouClass::Peak;
    let goal = if bank {
        s.vehicle.battery_kwh
    } else {
        s.vehicle.soc_target_kwh
    };
    (goal - s.soc_kwh).max(0.0)
}

/// A battery-side desire converted to grid-side power under the session's
/// physical limit.
fn charge_power_for(need_kwh: f64, s: &SessionView, obs: &Observation) -> f64 {
    if need_kwh <= 0.0 {
        return 0.0;
    }
    let need_grid_kwh = need_kwh / obs.charge_efficiency;
    crate::kwh_to_kw(need_grid_kwh, obs.slot_minutes).min(s.max_charge_kw)
}

/// The power that charges session `s` toward its target as fast as its own
/// limits allow this slot (no site-level considerations).
fn charge_power_toward_target(s: &SessionView, obs: &Observation) -> f64 {
    charge_power_for(s.remaining_need_kwh(), s, obs)
}

/// Allocate charge power in priority order under the slot's power headroom,
/// force-charge sessions whose target would otherwise become unreachable,
/// then (if `v2b`) discharge surplus energy to pull net load down to the DR
/// commitment.
fn prioritized_allocation(obs: &Observation, order: &[usize], v2b: bool) -> Vec<Setpoint> {
    // Headroom for the charger fleet: the tighter of the site cap and, during
    // a DR window, the firm service level, minus the inflexible building load.
    let cap_kw = match (obs.site_cap_kw, obs.dr_fsl_kw) {
        (Some(c), Some(f)) => Some(c.min(f)),
        (Some(c), None) => Some(c),
        (None, Some(f)) => Some(f),
        (None, None) => None,
    };
    let mut headroom_kw = cap_kw.map(|c| (c - obs.building_load_kw).max(0.0));

    let mut setpoints: Vec<Setpoint> = Vec::with_capacity(obs.sessions.len());
    for &i in order {
        let s = &obs.sessions[i];
        let mut p = charge_power_for(desired_kwh(s, obs, v2b), s, obs);
        if let Some(h) = headroom_kw.as_mut() {
            p = p.min(*h);
            *h -= p;
        }
        // Force-charge fallback (audit F6): if the departure target is no
        // longer reachable at full power in the remaining slots, the economic
        // headroom (DR firm level) yields to the service guarantee: charge
        // toward the target regardless and eat the window penalty. The
        // engine's *physical* limits (site cap, no-export) still apply.
        let target_rate = charge_power_toward_target(s, obs);
        if p < target_rate
            && s.laxity_slots(obs.slot, obs.slot_minutes, obs.charge_efficiency) <= 0.0
        {
            if let Some(h) = headroom_kw.as_mut() {
                // Return what this session had taken, then take the full rate.
                *h += p;
                *h = (*h - target_rate).max(0.0);
            }
            p = target_rate;
        }
        setpoints.push(Setpoint {
            session_index: s.index,
            power_kw: p,
        });
    }

    if v2b {
        if let Some(fsl) = obs.dr_fsl_kw {
            discharge_to_fsl(obs, order, &mut setpoints, fsl);
        }
    }
    setpoints
}

/// If the building's inflexible load alone exceeds the DR commitment, discharge
/// vehicles (reverse priority order, so the most time-constrained vehicles are
/// touched last) to bring net load down toward `fsl`. Each vehicle only gives
/// up energy it can recover before departure ([`discharge_budget_kwh`]).
fn discharge_to_fsl(obs: &Observation, order: &[usize], setpoints: &mut [Setpoint], fsl: f64) {
    let allocated: f64 = setpoints.iter().map(|sp| sp.power_kw).sum();
    let mut excess_kw = obs.building_load_kw + allocated - fsl;
    if excess_kw <= 0.0 {
        return;
    }
    for &i in order.iter().rev() {
        if excess_kw <= 0.0 {
            break;
        }
        let s = &obs.sessions[i];
        let sp = setpoints
            .iter_mut()
            .find(|sp| sp.session_index == s.index)
            .expect("setpoint exists for every ordered session");
        // Cancel planned charging first, but never below what a
        // force-charged (laxity <= 0) session needs: the service guarantee
        // outranks the window.
        let forced = s.laxity_slots(obs.slot, obs.slot_minutes, obs.charge_efficiency) <= 0.0;
        if sp.power_kw > 0.0 && !forced {
            let cut = sp.power_kw.min(excess_kw);
            sp.power_kw -= cut;
            excess_kw -= cut;
            if excess_kw <= 0.0 {
                break;
            }
        }
        if s.max_discharge_kw <= 0.0 {
            continue;
        }
        // The budget is battery-side energy; a setpoint is building-side
        // power. A battery surplus S delivers only S * eta_d to the building,
        // so convert before turning it into a power bound (without this the
        // battery dips below the reserve by the conversion loss).
        let budget_building_kwh = discharge_budget_kwh(s, obs) * obs.discharge_efficiency;
        let budget_kw = crate::kwh_to_kw(budget_building_kwh, obs.slot_minutes);
        let p_dis = s.max_discharge_kw.min(budget_kw).min(excess_kw).max(0.0);
        if p_dis > 0.0 {
            sp.power_kw = -p_dis;
            excess_kw -= p_dis;
        }
    }
}

/// Battery energy session `s` may export this slot: only the surplus above
/// max(departure target, SoC floor). This is deliberately conservative: a
/// "borrow now, recharge later" budget would need to know the *future*
/// charging headroom, and inside a DR window that headroom is bound to zero
/// by the firm level, which can silently sacrifice departure targets when the
/// window abuts departure (a review probe demonstrated exactly that failure
/// with the previous full-recharge-rate budget). Surplus-only discharge can
/// never cause a target miss, whatever the future holds.
fn discharge_budget_kwh(s: &SessionView, _obs: &Observation) -> f64 {
    let reserved_kwh = s.vehicle.soc_target_kwh.max(s.vehicle.min_soc_kwh);
    (s.soc_kwh - reserved_kwh).max(0.0)
}
