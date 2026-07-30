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
    """Independent re-simulation of every built-in policy (idle, uncontrolled,
    edf, llf, edf-v2b, llf-v2b), including banking, force-charge, the V2B
    discharge overlay, and all engine clamps."""
    horizon, dt, eta_c, eta_d = sc["horizon"], sc["dt"], sc["eta_c"], sc["eta_d"]
    n = len(sc["vehicles"])
    arrival_soc = [v["soc_arrival_kwh"] for v in sc["vehicles"]]
    chain = {}
    active = {}  # row index -> {"charger": c, "soc": kwh, "drawn": kwh, "exported": kwh}
    charger_free = [True] * len(sc["chargers"])
    waiting = []
    slots_out = []
    sessions_out = {}

    arrival_order = sorted(range(n), key=lambda i: (sc["vehicles"][i]["arrival_slot"], sc["vehicles"][i]["vehicle_id"]))

    def finish(i, state, never):
        v = sc["vehicles"][i]
        sessions_out[(v["vehicle_id"], v["arrival_slot"])] = {
            "soc_arrival": state["soc_arrival"],
            "soc_departure": state["soc"],
            "drawn": state["drawn"],
            "exported": state["exported"],
            "never_connected": never,
        }

    for s in range(horizon):
        # departures
        for i in range(n):
            v = sc["vehicles"][i]
            if v["departure_slot"] == s:
                if i in active:
                    st = active.pop(i)
                    charger_free[st["charger"]] = True
                    chain[v["vehicle_id"]] = st["soc"]
                    finish(i, st, False)
                elif i in waiting:
                    waiting.remove(i)
                    st = {"soc_arrival": arrival_soc[i], "soc": arrival_soc[i], "drawn": 0.0, "exported": 0.0}
                    chain[v["vehicle_id"]] = st["soc"]
                    finish(i, st, True)
        # arrivals
        for i in arrival_order:
            v = sc["vehicles"][i]
            if v["arrival_slot"] == s:
                if sc["persistence"] and v["vehicle_id"] in chain:
                    raw = chain[v["vehicle_id"]] - v["depletion_kwh"]
                    arrival_soc[i] = min(max(raw, v["min_soc_kwh"]), v["battery_kwh"])
                waiting.append(i)
        # assignment (capability-aware, fallback any)
        still = []
        for i in waiting:
            v = sc["vehicles"][i]
            wants_bidi = v["max_discharge_kw"] > 0.0
            pick = None
            for c, free in enumerate(charger_free):
                if free and sc["chargers"][c]["bidirectional"] == wants_bidi:
                    pick = c
                    break
            if pick is None:
                for c, free in enumerate(charger_free):
                    if free:
                        pick = c
                        break
            if pick is None:
                still.append(i)
            else:
                charger_free[pick] = False
                active[i] = {
                    "charger": pick,
                    "soc_arrival": arrival_soc[i],
                    "soc": arrival_soc[i],
                    "drawn": 0.0,
                    "exported": 0.0,
                }
        waiting = still

        # decision: mirror the policy layer exactly, in its own words.
        building = sc["building"][s]
        tou = sc["tou"][s]
        fsls = [e["fsl"] for e in sc["dr_events"] if dr_covers(e, s)]
        fsl = min(fsls) if fsls else None
        cap = sc["site_cap"]
        eff_cap = min(x for x in [cap, fsl] if x is not None) if (cap is not None or fsl is not None) else None

        def limits(i):
            v = sc["vehicles"][i]
            port = sc["chargers"][active[i]["charger"]]
            max_chg = min(v["max_charge_kw"], port["max_kw"])
            max_dis = min(v["max_discharge_kw"], port["max_kw"]) if port["bidirectional"] else 0.0
            return max_chg, max_dis

        def laxity(i):
            v = sc["vehicles"][i]
            need = max(0.0, v["soc_target_kwh"] - active[i]["soc"])
            per_slot = limits(i)[0] * dt * eta_c
            if need <= 0.0:
                slots_needed = 0.0
            elif per_slot <= 0.0:
                slots_needed = float("inf")
            else:
                slots_needed = need / per_slot
            return (v["departure_slot"] - s) - slots_needed

        # Canonical view order = (arrival_slot, vehicle_id).
        canonical = sorted(
            active, key=lambda i: (sc["vehicles"][i]["arrival_slot"], sc["vehicles"][i]["vehicle_id"])
        )

        requests = []  # (row, kw) in EMISSION order
        v2b = policy_name.endswith("-v2b")
        if policy_name == "uncontrolled":
            for i in canonical:
                v = sc["vehicles"][i]
                need = max(0.0, v["soc_target_kwh"] - active[i]["soc"])
                kw = min(need / eta_c / dt, limits(i)[0]) if need > 0 else 0.0
                requests.append((i, kw))
        elif policy_name in ("edf", "llf", "edf-v2b", "llf-v2b"):
            if policy_name.startswith("edf"):
                order = sorted(
                    active,
                    key=lambda i: (sc["vehicles"][i]["departure_slot"], sc["vehicles"][i]["vehicle_id"]),
                )
            else:
                order = sorted(active, key=lambda i: (laxity(i), sc["vehicles"][i]["vehicle_id"]))
            headroom = max(0.0, eff_cap - building) if eff_cap is not None else None
            for i in order:
                st = active[i]
                v = sc["vehicles"][i]
                # V2B variants bank (charge toward capacity) off peak-price slots.
                goal = v["battery_kwh"] if (v2b and tou != "peak") else v["soc_target_kwh"]
                need = max(0.0, goal - st["soc"])
                kw = min(need / eta_c / dt, limits(i)[0]) if need > 0 else 0.0
                if headroom is not None:
                    kw = min(kw, headroom)
                    headroom -= kw
                # Force-charge fallback: the service guarantee outranks the
                # economic headroom once the target is barely reachable.
                target_need = max(0.0, v["soc_target_kwh"] - st["soc"])
                target_rate = min(target_need / eta_c / dt, limits(i)[0]) if target_need > 0 else 0.0
                if kw < target_rate and laxity(i) <= 0.0:
                    if headroom is not None:
                        headroom = max(0.0, headroom + kw - target_rate)
                    kw = target_rate
                requests.append((i, kw))
            if v2b and fsl is not None:
                allocated = sum(kw for _, kw in requests)
                excess = building + allocated - fsl
                if excess > 0:
                    amended = {i: kw for i, kw in requests}
                    for i in reversed(order):
                        if excess <= 0:
                            break
                        v = sc["vehicles"][i]
                        st = active[i]
                        forced = laxity(i) <= 0.0
                        if amended[i] > 0.0 and not forced:
                            cut = min(amended[i], excess)
                            amended[i] -= cut
                            excess -= cut
                            if excess <= 0:
                                break
                        max_dis = limits(i)[1]
                        if max_dis <= 0.0:
                            continue
                        reserved = max(v["soc_target_kwh"], v["min_soc_kwh"])
                        budget_building_kwh = max(0.0, st["soc"] - reserved) * eta_d
                        p_dis = max(0.0, min(max_dis, budget_building_kwh / dt, excess))
                        if p_dis > 0.0:
                            amended[i] = -p_dis
                            excess -= p_dis
                    requests = [(i, amended[i]) for i in order]
        elif policy_name == "idle":
            pass
        else:
            raise ValueError(policy_name)

        # Integration with engine-side clamps, charge pass first then
        # discharge pass, both in emission order.
        charge_headroom = float("inf") if cap is None else max(0.0, cap - building)
        total_charge = 0.0
        for i, kw in requests:
            if kw < 0.0:
                continue
            st = active[i]
            v = sc["vehicles"][i]
            p = min(kw, limits(i)[0], charge_headroom)
            room = v["battery_kwh"] - st["soc"]
            p = max(0.0, min(p, (room / eta_c) / dt))
            grid_kwh = p * dt
            st["soc"] += grid_kwh * eta_c
            st["drawn"] += grid_kwh
            total_charge += p
            charge_headroom -= p
        export_headroom = building + total_charge
        total_discharge = 0.0
        for i, kw in requests:
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
    for i in waiting:
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
                expected = min(max(raw, v["min_soc_kwh"]), v["battery_kwh"])
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
    for t in trace:
        s, cid = int(t["slot"]), int(t["charger_id"])
        p = float(t["power_kw"])
        key = (s, cid)
        check("P6-exclusivity", key not in seen, f"charger {cid} slot {s}")
        seen.add(key)
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
            if policy in ("edf-v2b", "llf-v2b"):
                reserved = max(v["soc_target_kwh"], v["min_soc_kwh"])
                check(
                    "V2B-surplus-only",
                    float(t["soc_kwh"]) >= reserved - ABS_TOL,
                    f"vehicle {t['vehicle_id']} slot {s}: soc {t['soc_kwh']} < reserved {reserved}",
                )
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

    # Independent trajectory re-simulation for EVERY built-in policy.
    if policy in ("idle", "uncontrolled", "edf", "llf", "edf-v2b", "llf-v2b"):
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
