#!/usr/bin/env python3
"""Convert an OPTIMUS-format episode into an openv2b scenario directory.

Usage:
  python3 tools/convert_optimus.py <optimus_episode_dir> <output_scenario_dir>
      [--slot-minutes 15] [--demand-peak 11.67] [--no-same-day-clamp]

Input (OPTIMUS persistence layout, single building):
  cars.csv          static per-vehicle: capacity_kwh, soc (%), min/max_allowed_soc (%)
  sessions.csv      per-session: arrival timestamp, duration (s),
                    required_soc_at_depart (%), previous_day_external_use_soc (%)
  chargers.csv      directionality, charge_rates_kw as a "(min, max)" tuple
  building_load.csv datetime, power_kw (t=0 is midnight of the first row's day)
  grid_prices.csv   datetime, price_per_kwh, type (peak / off-peak / super off-peak)
  dso_commands.csv  optional: start/end datetimes, fsl (kW)

Mapping decisions (documented, deliberate):
  - SoC percent -> kWh via each car's capacity.
  - OPTIMUS's max_allowed_soc ceiling becomes the openv2b battery ceiling:
    battery_kwh = capacity * max_allowed/100 (charging can never exceed it in
    either simulator); min_allowed becomes min_soc_kwh.
  - Arrival is CEILED to a slot, departure (arrival + duration) FLOORED, and
    both are clamped to the arrival's calendar day (OPTIMUS convention),
    unless --no-same-day-clamp.
  - DR events get the OPTIMUS constants: penalty 6 $/kWh, incentive
    13.6 $/kW; baseline defaults to the advertised fsl (no counterfactual
    here; use plan_fsl for a committed baseline).
  - Demand charge: peak-TOU component only (OPTIMUS convention), default
    11.67 $/kW; facilities component 0.
"""

import argparse
import ast
import csv
import json
import math
import sys
from datetime import datetime
from pathlib import Path

PENALTY_USD_PER_KWH = 6.0
INCENTIVE_USD_PER_KW = 13.6


def read_rows(path: Path):
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def parse_dt(s: str) -> datetime:
    return datetime.strptime(s.strip(), "%Y-%m-%d %H:%M:%S")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("episode_dir", type=Path)
    ap.add_argument("out_dir", type=Path)
    ap.add_argument("--slot-minutes", type=float, default=15.0)
    ap.add_argument("--demand-peak", type=float, default=11.67)
    ap.add_argument("--no-same-day-clamp", action="store_true")
    args = ap.parse_args()
    ep, out = args.episode_dir, args.out_dir
    slot_s = args.slot_minutes * 60.0

    building = read_rows(ep / "building_load.csv")
    t0 = parse_dt(building[0]["datetime"])
    if t0.hour or t0.minute or t0.second:
        print(f"warning: building load starts at {t0}, not midnight", file=sys.stderr)

    def to_slot(dt: datetime) -> float:
        return (dt - t0).total_seconds() / slot_s

    horizon = int(round(to_slot(parse_dt(building[-1]["datetime"])))) + 1
    out.mkdir(parents=True, exist_ok=True)

    # building_load.csv -> slot,value
    with open(out / "building_load.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["slot", "value"])
        for r in building:
            w.writerow([int(round(to_slot(parse_dt(r["datetime"])))), float(r["power_kw"])])

    # grid_prices.csv -> slot,value,tou
    tou_map = {"peak": "peak", "off-peak": "off-peak", "super off-peak": "super-off-peak"}
    with open(out / "grid_prices.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["slot", "value", "tou"])
        for r in read_rows(ep / "grid_prices.csv"):
            slot = max(0, int(math.ceil(to_slot(parse_dt(r["datetime"])) - 1e-9)))
            w.writerow([slot, float(r["price_per_kwh"]), tou_map[r["type"].strip()]])

    # chargers.csv
    with open(out / "chargers.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["charger_id", "max_kw", "bidirectional"])
        for r in read_rows(ep / "chargers.csv"):
            lo, hi = ast.literal_eval(r["charge_rates_kw"])
            w.writerow([int(float(r["charger_id"])), float(hi), str(lo < 0).lower()])

    # cars + sessions -> vehicles.csv (one row per session)
    cars = {int(float(r["car_id"])): r for r in read_rows(ep / "cars.csv")}
    sessions = sorted(
        read_rows(ep / "sessions.csv"),
        key=lambda r: (int(float(r["car_id"])), parse_dt(r["arrival"])),
    )
    with open(out / "vehicles.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "vehicle_id", "arrival_slot", "departure_slot", "battery_kwh",
                "soc_arrival_kwh", "soc_target_kwh", "max_charge_kw",
                "max_discharge_kw", "min_soc_kwh", "depletion_kwh",
            ]
        )
        port = read_rows(ep / "chargers.csv")[0]
        lo, hi = ast.literal_eval(port["charge_rates_kw"])
        for s in sessions:
            car = cars[int(float(s["car_id"]))]
            cap = float(car["capacity_kwh"])
            arr_dt = parse_dt(s["arrival"])
            dep_dt_s = arr_dt.timestamp() + float(s["duration"])
            arr = int(math.ceil(to_slot(arr_dt) - 1e-9))
            dep = int(math.floor((dep_dt_s - t0.timestamp()) / slot_s + 1e-9))
            if not args.no_same_day_clamp:
                # OPTIMUS: a session never spills past the last slot of its day.
                day_end = (int(to_slot(arr_dt)) // 96) * 96 + 96
                dep = min(dep, day_end - 0)  # departure_slot is exclusive
            dep = min(dep, horizon)
            if dep <= arr:
                print(f"skip zero-length session car {s['car_id']} at {s['arrival']}", file=sys.stderr)
                continue
            w.writerow(
                [
                    int(float(s["car_id"])),
                    arr,
                    dep,
                    round(cap * float(car["max_allowed_soc"]) / 100.0, 6),
                    round(cap * float(car["soc"]) / 100.0, 6),
                    round(cap * float(s["required_soc_at_depart"]) / 100.0, 6),
                    float(hi),
                    -float(lo) if lo < 0 else 0.0,
                    round(cap * float(car["min_allowed_soc"]) / 100.0, 6),
                    round(cap * float(s["previous_day_external_use_soc"]) / 100.0, 6),
                ]
            )

    # dso_commands.csv -> dr_events.csv (optional)
    dr_file = None
    dso = ep / "dso_commands.csv"
    if dso.exists():
        rows = read_rows(dso)
        if rows:
            dr_file = "dr_events.csv"
            with open(out / dr_file, "w", newline="") as f:
                w = csv.writer(f)
                w.writerow(
                    ["start_slot", "end_slot", "fsl_kw", "penalty_usd_per_kwh",
                     "incentive_usd_per_kw", "baseline_kw"]
                )
                for r in rows:
                    start = int(to_slot(parse_dt(r["start_datetime"])))
                    end = int(to_slot(parse_dt(r["end_datetime"])))
                    fsl = float(r["fsl"])
                    w.writerow([start, min(end, horizon - 1), fsl,
                                PENALTY_USD_PER_KWH, INCENTIVE_USD_PER_KW, fsl])

    manifest = {
        "slot_minutes": args.slot_minutes,
        "horizon_slots": horizon,
        "charge_efficiency": 1.0,
        "discharge_efficiency": 1.0,
        "demand_charge_usd_per_kw": 0.0,
        "demand_charge_peak_usd_per_kw": args.demand_peak,
        "persistence": True,
    }
    if dr_file:
        manifest["dr_events_file"] = dr_file
    (out / "scenario.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"wrote {out} ({horizon} slots, {len(sessions)} sessions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
