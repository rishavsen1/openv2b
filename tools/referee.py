#!/usr/bin/env python3
"""Independent verification referee for openv2b simulation outputs.

Usage: python3 tools/referee.py <scenario_dir> <output_dir>

Recomputes, from the scenario inputs and the simulator's outputs, every metric
the simulator reports, using an independent implementation (Python stdlib
only; plain loops; its own copy of every convention, including the (start,end]
DR window rule). For the `uncontrolled` and `edf` policies it additionally
re-simulates the full trajectory from scratch and requires slot-exact
agreement. For the V2B policies it enforces bound properties (discharge only
from surplus above max(target, floor), checked per trace row).

Policy-agnostic checks (they hold for the optimizing policies too, which are
otherwise only bound-checked): per-DR-event settlement against the peak inside
that event's window, per-session reconciliation of the metered energies and
the departure SoC against that session's OWN trace rows, and an optional
planner ramp bound.

THE RAMP BOUND IS OPT-IN, via the manifest field `planner_ramp_kwh_per_slot`
(kWh per slot; divided by the slot length to get kW). Declare it ONLY for a
run whose applied trajectory came from a SINGLE ramp-limited plan (a
solve-once plan replayed through the engine). The heuristics have no ramp at
all, and a RECEDING controller escapes the bound by construction: its
consecutive committed slots come from two different solves and are tied only
*within* each plan (measured on `scenario-mpc`: 15 kW slot-to-slot swings
under a 1.25 kWh/slot = 5 kW ramp). Setting the field for such a run is a
declaration error, and the referee will say so loudly.

Exit code 0 = all checks pass; 1 = at least one FAIL (details on stdout).
"""

import csv
import json
import math
import sys
from pathlib import Path

REL_TOL = 1e-9
ABS_TOL = 1e-6

failures = []


def check(name: str, ok: bool, detail: str = "") -> None:
    if not ok:
        failures.append(name)
        print(f"FAIL {name} {detail}")


def close(a: float, b: float) -> bool:
    return math.isclose(a, b, rel_tol=REL_TOL, abs_tol=ABS_TOL)


# ---------------------------------------------------------------- inputs


def read_csv(path: Path):
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def densify(rows, horizon, value_key="value"):
    """Step-and-hold densification, independently implemented."""
    series = [0.0] * horizon
    pts = sorted((int(r["slot"]), float(r[value_key])) for r in rows)
    current, i = 0.0, 0
    for s in range(horizon):
        while i < len(pts) and pts[i][0] <= s:
            current = pts[i][1]
            i += 1
        series[s] = current
    return series


def densify_tou(rows, horizon):
    series = ["off-peak"] * horizon
    # Stable sort on the slot only: None tou cells must not participate in
    # comparisons (they hold the previous class).
    pts = sorted(((int(r["slot"]), r.get("tou") or None) for r in rows), key=lambda t: t[0])
    current, i = "off-peak", 0
    for s in range(horizon):
        while i < len(pts) and pts[i][0] <= s:
            if pts[i][1]:
                current = pts[i][1]
            i += 1
        series[s] = current
    return series


def load_scenario(d: Path):
    m = json.loads((d / "scenario.json").read_text())
    horizon = m["horizon_slots"]
    vehicles = read_csv(d / m.get("vehicles_file", "vehicles.csv"))
    for v in vehicles:
        for k in v:
            v[k] = float(v[k]) if "." in str(v[k]) or "e" in str(v[k]) else v[k]
    price_rows = read_csv(d / m.get("prices_file", "grid_prices.csv"))
    scenario = {
        "manifest": m,
        "horizon": horizon,
        "dt": m["slot_minutes"] / 60.0,
        "eta_c": m.get("charge_efficiency", 1.0),
        "eta_d": m.get("discharge_efficiency", 1.0),
        "persistence": m.get("persistence", True),
        "site_cap": m.get("site_cap_kw"),
        "vehicles": [
            {
                "vehicle_id": int(r["vehicle_id"]),
                "arrival_slot": int(r["arrival_slot"]),
                "departure_slot": int(r["departure_slot"]),
                "battery_kwh": float(r["battery_kwh"]),
                "soc_arrival_kwh": float(r["soc_arrival_kwh"]),
                "soc_target_kwh": float(r["soc_target_kwh"]),
                "max_charge_kw": float(r["max_charge_kw"]),
                "max_discharge_kw": float(r.get("max_discharge_kw") or 0.0),
                "min_soc_kwh": float(r.get("min_soc_kwh") or 0.0),
                "max_soc_kwh": float(r["max_soc_kwh"]) if r.get("max_soc_kwh") else None,
                "depletion_kwh": float(r.get("depletion_kwh") or 0.0),
            }
            for r in read_csv(d / m.get("vehicles_file", "vehicles.csv"))
        ],
        "chargers": [
            {
                "charger_id": int(r["charger_id"]),
                "max_kw": float(r["max_kw"]),
                "bidirectional": str(r["bidirectional"]).strip().lower() == "true",
            }
            for r in read_csv(d / m.get("chargers_file", "chargers.csv"))
        ],
        "building": densify(read_csv(d / m.get("building_file", "building_load.csv")), horizon),
        "price": densify(price_rows, horizon),
        "tou": densify_tou(price_rows, horizon),
        "dr_events": [
            {
                "start": int(r["start_slot"]),
                "end": int(r["end_slot"]),
                "fsl": float(r["fsl_kw"]),
                "penalty_rate": float(r["penalty_usd_per_kwh"]),
                "incentive_rate": float(r.get("incentive_usd_per_kw") or 0.0),
                "baseline": float(r.get("baseline_kw") or 0.0),
            }
            for r in (read_csv(d / m["dr_events_file"]) if m.get("dr_events_file") else [])
        ],
    }
    scenario["chargers"].sort(key=lambda c: c["charger_id"])
    return scenario


def dr_covers(event, s: int) -> bool:
    """The referee's OWN copy of the (start, end] convention."""
    return event["start"] < s <= event["end"]


def m_manifest_demand(sc, peak: float, peak_tou: float) -> float:
    m = sc["manifest"]
    return m.get("demand_charge_usd_per_kw", 0.0) * peak + m.get(
        "demand_charge_peak_usd_per_kw", 0.0
    ) * peak_tou


# ------------------------------------------------------- independent sim


def resimulate(sc, policy_name: str):
    """Independent re-simulation of every built-in policy: idle, uncontrolled,
    and the OPTIMUS ports (policy-0/1/2, edf, llf) including the threshold
    budget walk, taper, force-charge, ratchet, and all engine clamps."""
    horizon, dt, eta_c, eta_d = sc["horizon"], sc["dt"], sc["eta_c"], sc["eta_d"]
    n = len(sc["vehicles"])
    arrival_soc = [v["soc_arrival_kwh"] for v in sc["vehicles"]]
    chain = {}
    active = {}  # row index -> {"charger": c, "soc": kwh, "drawn": kwh, "exported": kwh}
    charger_free = [True] * len(sc["chargers"])
    slots_out = []
    sessions_out = {}
    dropped = set()

    def ceiling(v):
        c = v.get("max_soc_kwh")
        return c if c is not None else v["battery_kwh"]

    # EDF/LLF threshold (historical_max_load): manifest seed or the reference
    # fallback 0.8 * max building load; ratchets monotonically upward.
    threshold = sc["manifest"].get("heuristic_threshold_kw")
    if threshold is None:
        threshold = 0.8 * max(sc["building"])

    def taper(v, soc_kwh, rate_kw):
        """The reference get_rate: >90%-of-true-capacity charge taper (exact
        comparisons), hard discharge floor, no shaping on negatives."""
        soc = soc_kwh / v["battery_kwh"] * 100.0
        max_soc = ceiling(v) / v["battery_kwh"] * 100.0
        min_soc = v["min_soc_kwh"] / v["battery_kwh"] * 100.0
        if rate_kw > 0.0:
            if soc <= max_soc:
                return rate_kw if soc <= 90.0 else -rate_kw / 10.0 * (soc - 90.0) + rate_kw
            return 0.0
        if rate_kw < 0.0:
            return rate_kw if soc >= min_soc else 0.0
        return 0.0

    def isclose_pct(a, b):
        return abs(a - b) <= 0.1 + 1e-5 * abs(b)

    def finish(i, state, never):
        v = sc["vehicles"][i]
        sessions_out[(v["vehicle_id"], v["arrival_slot"])] = {
            "soc_arrival": state["soc_arrival"],
            "soc_departure": state["soc"],
            "drawn": state["drawn"],
            "exported": state["exported"],
            "never_connected": never,
        }

    arrival_rows = sorted(range(n), key=lambda i: (sc["vehicles"][i]["arrival_slot"], sc["vehicles"][i]["vehicle_id"]))

    for s in range(horizon):
        # departures (before arrivals: same-slot chain handoffs)
        for i in range(n):
            v = sc["vehicles"][i]
            if v["departure_slot"] == s:
                if i in active:
                    st = active.pop(i)
                    charger_free[st["charger"]] = True
                    chain[v["vehicle_id"]] = st["soc"]
                    finish(i, st, False)
                elif i in dropped:
                    dropped.discard(i)
                    st = {"soc_arrival": arrival_soc[i], "soc": arrival_soc[i], "drawn": 0.0, "exported": 0.0}
                    chain[v["vehicle_id"]] = st["soc"]
                    finish(i, st, True)
        # arrivals: persistence chain resolves here (clamped to the ceiling)
        arrivals_now = []
        for i in arrival_rows:
            v = sc["vehicles"][i]
            if v["arrival_slot"] == s:
                if sc["persistence"] and v["vehicle_id"] in chain:
                    raw = chain[v["vehicle_id"]] - v["depletion_kwh"]
                    arrival_soc[i] = min(max(raw, v["min_soc_kwh"]), ceiling(v))
                arrivals_now.append(i)
        # assignment, reference semantics: ascending vehicle id; every car
        # prefers a bidirectional port (lowest id ties); no vacancy -> DROPPED
        # permanently, never retried.
        for i in sorted(arrivals_now, key=lambda i: sc["vehicles"][i]["vehicle_id"]):
            vacant = [c for c, free in enumerate(charger_free) if free]
            vacant.sort(key=lambda c: (not sc["chargers"][c]["bidirectional"], c))
            if vacant:
                c = vacant[0]
                charger_free[c] = False
                active[i] = {
                    "charger": c,
                    "soc_arrival": arrival_soc[i],
                    "soc": arrival_soc[i],
                    "drawn": 0.0,
                    "exported": 0.0,
                }
            else:
                dropped.add(i)

        # decision: mirror the ported policies exactly, in the referee's words.
        building = sc["building"][s]
        tou = sc["tou"][s]
        cap = sc["site_cap"]

        def limits(i):
            v = sc["vehicles"][i]
            port = sc["chargers"][active[i]["charger"]]
            max_chg = min(v["max_charge_kw"], port["max_kw"])
            max_dis = min(v["max_discharge_kw"], port["max_kw"]) if port["bidirectional"] else 0.0
            return max_chg, max_dis

        # Canonical view order = (arrival_slot, vehicle_id): the emission
        # order for the per-session policies.
        canonical = sorted(
            active, key=lambda i: (sc["vehicles"][i]["arrival_slot"], sc["vehicles"][i]["vehicle_id"])
        )

        requests = []  # (row, kw) in EMISSION order
        if policy_name == "idle":
            pass
        elif policy_name == "uncontrolled":
            for i in canonical:
                v = sc["vehicles"][i]
                need = max(0.0, v["soc_target_kwh"] - active[i]["soc"])
                kw = min(need / eta_c / dt, limits(i)[0]) if need > 0 else 0.0
                requests.append((i, kw))
        elif policy_name == "policy-0":
            for i in canonical:
                v = sc["vehicles"][i]
                st = active[i]
                soc = st["soc"] / v["battery_kwh"] * 100.0
                req = v["soc_target_kwh"] / v["battery_kwh"] * 100.0
                rate = 0.0
                if soc < req and not isclose_pct(soc, req):
                    hours = (v["departure_slot"] - s) * dt
                    rate = (v["soc_target_kwh"] - st["soc"]) / hours
                    rate = min(rate, limits(i)[0])
                    rate = max(rate, 0.0)
                    rate = taper(v, st["soc"], rate)
                requests.append((i, rate))
        elif policy_name in ("policy-1", "policy-2"):
            for i in canonical:
                v = sc["vehicles"][i]
                st = active[i]
                soc = st["soc"] / v["battery_kwh"] * 100.0
                mx = ceiling(v) / v["battery_kwh"] * 100.0
                if soc < mx and not isclose_pct(soc, mx):
                    charge_ok = (
                        tou in ("off-peak", "super-off-peak")
                        if policy_name == "policy-1"
                        else tou == "off-peak"
                    )
                    rate = taper(v, st["soc"], limits(i)[0]) if charge_ok else 0.0
                    requests.append((i, rate))
            if policy_name == "policy-1":
                for i in canonical:
                    v = sc["vehicles"][i]
                    st = active[i]
                    soc = st["soc"] / v["battery_kwh"] * 100.0
                    req = v["soc_target_kwh"] / v["battery_kwh"] * 100.0
                    if soc > req and not isclose_pct(soc, req):
                        rate = taper(v, st["soc"], -limits(i)[1]) if tou == "peak" else 0.0
                        # second loop overwrites the first (last-wins dedup)
                        requests.append((i, rate))
        elif policy_name in ("edf", "llf"):
            # Eligibility: STRICT (no tolerance). Peak: below target; else
            # below ceiling.
            elig = []
            for i in canonical:
                v = sc["vehicles"][i]
                st = active[i]
                bound = v["soc_target_kwh"] if tou == "peak" else ceiling(v)
                if st["soc"] < bound:
                    elig.append(i)
            rows = []
            for i in elig:
                v = sc["vehicles"][i]
                st = active[i]
                need = v["soc_target_kwh"] - st["soc"]  # SIGNED
                tl = (v["departure_slot"] - s) * dt * 3600.0
                min_rate = need / (tl / 3600.0)
                if min_rate in (float("inf"), float("-inf")):
                    min_rate = 0.0
                if policy_name == "edf":
                    # IEEE division like the engine/numpy: x/0 -> +/-inf, 0/0 -> NaN.
                    num = 100.0 * need * limits(i)[0]
                    den = (threshold - building) * tl
                    if den == 0.0:
                        key = float("nan") if num == 0.0 else math.copysign(float("inf"), num)
                    else:
                        key = num / den
                else:
                    key = tl
                rows.append((i, min_rate, tl, key))
            reverse = policy_name == "edf"
            rows.sort(key=lambda r: (r[3] != r[3], -r[3] if reverse else r[3], sc["vehicles"][r[0]]["vehicle_id"]))
            capacity = threshold - building
            used_power = 0.0
            served = set()
            for i, min_rate, tl, _ in rows:
                if capacity <= 0.0:
                    break
                v = sc["vehicles"][i]
                st = active[i]
                rate = min_rate
                original = rate
                if used_power + rate > capacity:
                    rate = min(rate, capacity)
                rate = min(rate, limits(i)[0])
                rate = taper(v, st["soc"], rate)
                requests.append((i, rate))
                if rate >= original:
                    served.add(v["vehicle_id"])
                used_power += rate
                capacity -= rate
            for i, min_rate, tl, _ in rows:
                v = sc["vehicles"][i]
                if tl < 3600.0 and v["vehicle_id"] not in served:
                    st = active[i]
                    rate = min(min_rate, limits(i)[0]) if min_rate > 0 else max(min_rate, -limits(i)[1])
                    rate = taper(v, st["soc"], rate)
                    requests.append((i, rate))  # last-wins overwrite
                    used_power += rate
            if building + used_power > threshold:
                threshold = building + used_power
        else:
            raise ValueError(policy_name)

        # Integration with engine-side clamps: last-wins dedup, charge pass
        # first (site-cap headroom, ceiling room), then discharge pass
        # (no-export headroom, floor).
        deduped = {}
        order_seq = []
        for i, kw in requests:
            if i in deduped:
                order_seq.remove(i)
            deduped[i] = kw
            order_seq.append(i)
        charge_headroom = float("inf") if cap is None else max(0.0, cap - building)
        total_charge = 0.0
        for i in order_seq:
            kw = deduped[i]
            if kw < 0.0:
                continue
            st = active[i]
            v = sc["vehicles"][i]
            p = min(kw, limits(i)[0], charge_headroom)
            room = ceiling(v) - st["soc"]
            p = max(0.0, min(p, (room / eta_c) / dt))
            grid_kwh = p * dt
            st["soc"] += grid_kwh * eta_c
            st["drawn"] += grid_kwh
            total_charge += p
            charge_headroom -= p
        export_headroom = building + total_charge
        total_discharge = 0.0
        for i in order_seq:
            kw = deduped[i]
            if kw >= 0.0:
                continue
            st = active[i]
            v = sc["vehicles"][i]
            p = min(-kw, limits(i)[1], export_headroom)
            max_building_kwh = max(0.0, st["soc"] - v["min_soc_kwh"]) * eta_d
            p = max(0.0, min(p, max_building_kwh / dt))
            building_kwh = p * dt
            st["soc"] -= building_kwh / eta_d
            st["exported"] += building_kwh
            total_discharge += p
            export_headroom -= p

        slots_out.append(
            {
                "building_kw": building,
                "ev_charge_kw": total_charge,
                "ev_discharge_kw": total_discharge,
                "net_kw": building + total_charge - total_discharge,
            }
        )

    for i in list(active):
        finish(i, active.pop(i), False)
    for i in list(dropped):
        st = {"soc_arrival": arrival_soc[i], "soc": arrival_soc[i], "drawn": 0.0, "exported": 0.0}
        finish(i, st, True)
    return slots_out, sessions_out


# ------------------------------------------------------------- checking


def main() -> int:
    scenario_dir, output_dir = Path(sys.argv[1]), Path(sys.argv[2])
    sc = load_scenario(scenario_dir)
    horizon, dt = sc["horizon"], sc["dt"]
    eta_c, eta_d = sc["eta_c"], sc["eta_d"]

    slots = read_csv(output_dir / "slots.csv")
    sessions = read_csv(output_dir / "sessions.csv")
    trace = read_csv(output_dir / "trace.csv")
    summary = json.loads((output_dir / "summary.json").read_text())
    policy = summary["policy"]
    print(f"referee: {scenario_dir.name} / {policy}: {len(slots)} slots, {len(sessions)} sessions")

    check("slot-count", len(slots) == horizon)

    # M9-M14: per-slot identities and echoes.
    for r in slots:
        s = int(r["slot"])
        building = float(r["building_kw"])
        charge = float(r["ev_charge_kw"])
        discharge = float(r["ev_discharge_kw"])
        net = float(r["net_kw"])
        check("M9-building-echo", close(building, sc["building"][s]), f"slot {s}")
        check("M13-price-echo", close(float(r["price_usd_per_kwh"]), sc["price"][s]), f"slot {s}")
        check("M12-net-identity", close(net, building + charge - discharge), f"slot {s}")
        check("M14-no-export", net >= -ABS_TOL, f"slot {s}: net {net}")
        check("R1-8-signs", charge >= -ABS_TOL and discharge >= -ABS_TOL, f"slot {s}")
        if sc["site_cap"] is not None:
            check(
                "R1-6-site-cap",
                net <= max(building, sc["site_cap"]) + ABS_TOL,
                f"slot {s}: net {net}",
            )

    # Bill recomputation (M18-M25).
    energy_usd = sum(max(float(r["net_kw"]), 0.0) * dt * sc["price"][int(r["slot"])] for r in slots)
    imported = sum(max(float(r["net_kw"]), 0.0) * dt for r in slots)
    peak = max(float(r["net_kw"]) for r in slots)
    peak_tou = max((float(r["net_kw"]) for r in slots if sc["tou"][int(r["slot"])] == "peak"), default=0.0)
    m = sc["manifest"]
    demand_fac = m.get("demand_charge_usd_per_kw", 0.0) * peak
    demand_peak = m.get("demand_charge_peak_usd_per_kw", 0.0) * peak_tou
    penalty = incentive = 0.0
    for e in sc["dr_events"]:
        covered = [r for r in slots if dr_covers(e, int(r["slot"]))]
        overflow = sum(max(float(r["net_kw"]) - e["fsl"], 0.0) * dt for r in covered)
        penalty += e["penalty_rate"] * overflow
        if covered and overflow <= 1e-9:
            incentive += e["incentive_rate"] * max(e["baseline"] - e["fsl"], 0.0)
    total = energy_usd + demand_fac + demand_peak + penalty - incentive
    b = summary["bill"]
    check("M18-imported", close(imported, b["energy_imported_kwh"]), f"{imported} vs {b['energy_imported_kwh']}")
    check("M19-energy-usd", close(energy_usd, b["energy_usd"]), f"{energy_usd} vs {b['energy_usd']}")
    check("M20-peak", close(peak, b["peak_net_kw"]))
    check("M20-peak-tou", close(peak_tou, b["peak_net_peak_tou_kw"]))
    check("M21-demand", close(demand_fac + demand_peak, b["demand_usd"]))
    check("M23-penalty", close(penalty, b["dr_penalty_usd"]), f"{penalty} vs {b['dr_penalty_usd']}")
    check("M24-incentive", close(incentive, b["dr_incentive_usd"]), f"{incentive} vs {b['dr_incentive_usd']}")
    check("M25-total", close(total, b["total_usd"]), f"{total} vs {b['total_usd']}")
    check("peak-tou-le-peak", peak_tou <= peak + ABS_TOL)
    check("M20-peak-is-attained", any(close(float(r["net_kw"]), peak) for r in slots))

    # Per-DR-event settlement, policy-agnostic: the peak net load INSIDE each
    # window decides whether that window overflowed at all, so it must agree
    # with the reported per-event overflow, penalty, and honored/not-honored
    # incentive. (The aggregate M23/M24 totals can hide two events whose
    # errors cancel.)
    settlements = b.get("dr_settlements", [])
    check(
        "dr-settlement-count",
        len(settlements) == len(sc["dr_events"]),
        f"{len(settlements)} settlements vs {len(sc['dr_events'])} events",
    )
    for e, st in zip(sc["dr_events"], settlements):
        window = f"({e['start']}, {e['end']}]"
        check("dr-window-echo", st["start_slot"] == e["start"] and st["end_slot"] == e["end"], window)
        check("dr-window-fsl-echo", close(float(st["fsl_kw"]), e["fsl"]), window)
        covered = [float(r["net_kw"]) for r in slots if dr_covers(e, int(r["slot"]))]
        window_peak = max(covered) if covered else 0.0
        overflow = sum(max(x - e["fsl"], 0.0) * dt for x in covered)
        honored = bool(covered) and overflow <= 1e-9
        check("dr-window-covered", len(covered) == e["end"] - e["start"], window)
        check("dr-window-peak-le-peak", window_peak <= peak + ABS_TOL, f"{window}: {window_peak} > {peak}")
        check(
            "dr-window-peak-vs-overflow",
            (window_peak > e["fsl"] + ABS_TOL) == (overflow > 1e-9),
            f"{window}: peak-in-window {window_peak} vs fsl {e['fsl']}, overflow {overflow}",
        )
        check(
            "dr-window-overflow",
            close(overflow, float(st["overflow_kwh"])),
            f"{window}: {overflow} vs {st['overflow_kwh']}",
        )
        check(
            "dr-window-penalty",
            close(e["penalty_rate"] * overflow, float(st["penalty_usd"])),
            f"{window}: {e['penalty_rate'] * overflow} vs {st['penalty_usd']}",
        )
        expected_incentive = (
            e["incentive_rate"] * max(e["baseline"] - e["fsl"], 0.0) if honored else 0.0
        )
        check(
            "dr-window-incentive",
            close(expected_incentive, float(st["incentive_usd"])),
            f"{window}: {expected_incentive} vs {st['incentive_usd']}",
        )

    # M1-M8: per-session checks.
    vrows = {(v["vehicle_id"], v["arrival_slot"]): v for v in sc["vehicles"]}
    by_vehicle = {}
    for r in sessions:
        key = (int(r["vehicle_id"]), int(r["arrival_slot"]))
        v = vrows[key]
        arr, dep = float(r["soc_arrival_kwh"]), float(r["soc_departure_kwh"])
        drawn, exported = float(r["energy_drawn_kwh"]), float(r["energy_exported_kwh"])
        check("M3-conservation", close(dep, arr + eta_c * drawn - exported / eta_d), f"session {key}")
        check("M5-missing", close(float(r["missing_kwh"]), max(v["soc_target_kwh"] - dep, 0.0)), f"{key}")
        check("M6-banked", close(float(r["banked_kwh"]), max(dep - v["soc_target_kwh"], 0.0)), f"{key}")
        check(
            "soc-bounds",
            v["min_soc_kwh"] - ABS_TOL <= dep <= v["battery_kwh"] + ABS_TOL,
            f"{key}",
        )
        if r["never_connected"] == "true":
            check("unserved-unchanged", close(arr, dep), f"{key}")
        by_vehicle.setdefault(key[0], []).append((int(r["arrival_slot"]), r))

    # M8: persistence chain identity + clamp accounting.
    if sc["persistence"]:
        for vid, sess in by_vehicle.items():
            sess.sort()
            for (_, prev), (arr_slot, cur) in zip(sess, sess[1:]):
                v = vrows[(vid, arr_slot)]
                raw = float(prev["soc_departure_kwh"]) - v["depletion_kwh"]
                cei = v["max_soc_kwh"] if v.get("max_soc_kwh") is not None else v["battery_kwh"]
                expected = min(max(raw, v["min_soc_kwh"]), cei)
                check(
                    "M8-chain",
                    close(float(cur["soc_arrival_kwh"]), expected),
                    f"vehicle {vid} arriving slot {arr_slot}",
                )
                check(
                    "M8-clamp-accounting",
                    close(float(cur["chain_clamped_kwh"]), max(expected - raw, 0.0)),
                    f"vehicle {vid} arriving slot {arr_slot}",
                )

    # P21 site energy balance: slots vs sessions.
    net_kwh = sum(float(r["net_kw"]) * dt for r in slots)
    building_kwh = sum(sc["building"][int(r["slot"])] * dt for r in slots)
    drawn_total = sum(float(r["energy_drawn_kwh"]) for r in sessions)
    exported_total = sum(float(r["energy_exported_kwh"]) for r in sessions)
    check(
        "P21-site-balance",
        math.isclose(net_kwh, building_kwh + drawn_total - exported_total, rel_tol=1e-9, abs_tol=1e-5),
        f"{net_kwh} vs {building_kwh + drawn_total - exported_total}",
    )

    # Summary totals echo the session sums.
    check("summary-drawn", close(summary["energy_drawn_kwh"], drawn_total))
    check("summary-exported", close(summary["energy_exported_kwh"], exported_total))

    # Trace checks: charger exclusivity, per-port caps, aggregation match,
    # and (for V2B) the surplus-only discharge bound per trace row.
    chargers = {c["charger_id"]: c for c in sc["chargers"]}
    seen = set()
    agg_charge = [0.0] * horizon
    agg_discharge = [0.0] * horizon
    # (vehicle_id, arrival_slot) -> the session's own trace rows and energies.
    per_session = {}
    for t in trace:
        s, cid = int(t["slot"]), int(t["charger_id"])
        p = float(t["power_kw"])
        key = (s, cid)
        check("P6-exclusivity", key not in seen, f"charger {cid} slot {s}")
        seen.add(key)
        sess_key = (int(t["vehicle_id"]), int(t["arrival_slot"]))
        ps = per_session.setdefault(sess_key, {"drawn": 0.0, "exported": 0.0, "rows": []})
        if p >= 0:
            ps["drawn"] += p * dt
        else:
            ps["exported"] += -p * dt
        ps["rows"].append((s, p, float(t["soc_kwh"])))
        check("per-port-cap", abs(p) <= chargers[cid]["max_kw"] + ABS_TOL, f"charger {cid} slot {s}")
        if p >= 0:
            agg_charge[s] += p
        else:
            agg_discharge[s] -= p
            v = vrows[(int(t["vehicle_id"]), int(t["arrival_slot"]))]
            # Surplus-only discharge is the HEURISTICS' contract. Optimizing
            # policies (mpc) legitimately borrow below the target mid-session
            # and recover via their reachability constraint; for them the
            # floor and the target-met checks (M3/M5) are the guarantees.
            # (the deleted V2B-overlay heuristics carried a surplus-only
            # contract; the OPTIMUS ports discharge via metered channels and
            # are bound by the floor + resim equality instead)
            check(
                "V2B-floor",
                float(t["soc_kwh"]) >= v["min_soc_kwh"] - ABS_TOL,
                f"vehicle {t['vehicle_id']} slot {s}: below floor",
            )
            check("bidi-port", chargers[cid]["bidirectional"], f"discharge on unidirectional charger {cid}")
    for r in slots:
        s = int(r["slot"])
        check("trace-agg-charge", close(agg_charge[s], float(r["ev_charge_kw"])), f"slot {s}")
        check("trace-agg-discharge", close(agg_discharge[s], float(r["ev_discharge_kw"])), f"slot {s}")

    # PER-SESSION trace reconciliation (policy-agnostic). The aggregate
    # balance P21 sums over sessions, so one session over-reporting what
    # another under-reports cancels; these tie EACH session's reported
    # energies, occupancy window, and departure SoC to its OWN trace rows.
    for r in sessions:
        key = (int(r["vehicle_id"]), int(r["arrival_slot"]))
        v = vrows[key]
        ps = per_session.get(key)
        if ps is None:
            check(
                "trace-session-rows",
                r["never_connected"] == "true"
                and close(float(r["energy_drawn_kwh"]), 0.0)
                and close(float(r["energy_exported_kwh"]), 0.0),
                f"{key}: no trace rows but the session drew or exported energy",
            )
            continue
        rows = sorted(ps["rows"])
        check(
            "trace-session-drawn",
            close(ps["drawn"], float(r["energy_drawn_kwh"])),
            f"{key}: trace {ps['drawn']} vs session {r['energy_drawn_kwh']}",
        )
        check(
            "trace-session-exported",
            close(ps["exported"], float(r["energy_exported_kwh"])),
            f"{key}: trace {ps['exported']} vs session {r['energy_exported_kwh']}",
        )
        check(
            "trace-session-final-soc",
            close(rows[-1][2], float(r["soc_departure_kwh"])),
            f"{key}: last trace SoC {rows[-1][2]} vs departure {r['soc_departure_kwh']}",
        )
        expected_slots = list(range(v["arrival_slot"], min(v["departure_slot"], horizon)))
        check(
            "trace-session-window",
            [x[0] for x in rows] == expected_slots,
            f"{key}: traced slots {[x[0] for x in rows][:4]}... vs occupancy {expected_slots[:4]}...",
        )
        # The trace's own SoC recursion, independent of the session row.
        soc = float(r["soc_arrival_kwh"])
        for s, p, soc_end in rows:
            soc += (p * dt * eta_c) if p >= 0 else (p * dt / eta_d)
            check("trace-session-soc-recursion", close(soc, soc_end), f"{key} slot {s}")

    # Ramp bound: OPT-IN via the manifest (see the module docstring). The
    # heuristics have no ramp, and a receding controller's committed slots are
    # tied only within one plan, so this is never assumed.
    ramp_kwh_per_slot = sc["manifest"].get("planner_ramp_kwh_per_slot")
    if ramp_kwh_per_slot is not None:
        limit_kw = float(ramp_kwh_per_slot) / dt
        for key in sorted(per_session):
            rows = sorted(per_session[key]["rows"])
            for (s0, p0, _), (s1, p1, _) in zip(rows, rows[1:]):
                if s1 != s0 + 1:
                    continue
                check(
                    "ramp-bound",
                    abs(p1 - p0) <= limit_kw + ABS_TOL,
                    f"session {key} slots {s0}->{s1}: |{p1} - {p0}| kW exceeds {limit_kw} kW",
                )

    # Independent trajectory re-simulation for EVERY built-in policy.
    if policy in ("idle", "uncontrolled", "policy-0", "policy-1", "policy-2", "edf", "llf"):
        my_slots, my_sessions = resimulate(sc, policy)
        for s, (mine, theirs) in enumerate(zip(my_slots, slots)):
            check(
                "resim-slot",
                close(mine["net_kw"], float(theirs["net_kw"]))
                and close(mine["ev_charge_kw"], float(theirs["ev_charge_kw"]))
                and close(mine["ev_discharge_kw"], float(theirs["ev_discharge_kw"])),
                f"slot {s}: mine ({mine['ev_charge_kw']}, {mine['ev_discharge_kw']}) vs engine "
                f"({theirs['ev_charge_kw']}, {theirs['ev_discharge_kw']})",
            )
        for r in sessions:
            key = (int(r["vehicle_id"]), int(r["arrival_slot"]))
            mine = my_sessions[key]
            check("resim-soc", close(mine["soc_departure"], float(r["soc_departure_kwh"])), f"{key}")
            check("resim-drawn", close(mine["drawn"], float(r["energy_drawn_kwh"])), f"{key}")
            check("resim-exported", close(mine["exported"], float(r["energy_exported_kwh"])), f"{key}")
            check(
                "resim-unserved",
                mine["never_connected"] == (r["never_connected"] == "true"),
                f"{key}",
            )
        # F14: derive the bill from the referee's OWN net series too, so the
        # billing check is not circular over the engine's slots.csv.
        my_energy = sum(max(m["net_kw"], 0.0) * dt * sc["price"][s] for s, m in enumerate(my_slots))
        my_peak = max(m["net_kw"] for m in my_slots)
        my_peak_tou = max(
            (m["net_kw"] for s, m in enumerate(my_slots) if sc["tou"][s] == "peak"), default=0.0
        )
        my_penalty = my_incentive = 0.0
        for e in sc["dr_events"]:
            covered = [m for s, m in enumerate(my_slots) if dr_covers(e, s)]
            overflow = sum(max(m["net_kw"] - e["fsl"], 0.0) * dt for m in covered)
            my_penalty += e["penalty_rate"] * overflow
            if covered and overflow <= 1e-9:
                my_incentive += e["incentive_rate"] * max(e["baseline"] - e["fsl"], 0.0)
        my_total = (
            my_energy
            + m_manifest_demand(sc, my_peak, my_peak_tou)
            + my_penalty
            - my_incentive
        )
        check("resim-bill-total", close(my_total, b["total_usd"]), f"{my_total} vs {b['total_usd']}")

    if failures:
        print(f"\nREFEREE: {len(failures)} FAILURES in {scenario_dir.name}/{policy}")
        return 1
    print(f"REFEREE: all checks passed for {scenario_dir.name}/{policy}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
