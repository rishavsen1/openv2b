#!/usr/bin/env python3
"""Full verification campaign driver.

Runs every policy over the month datasets (lossless, lossy, persistence-off),
checks two-process determinism (SHA-256 over all output files), referees every
run, and writes an aggregated results table to reports/month_results.md.

Usage: python3 tools/run_verification.py [--workdir DIR]
"""

import argparse
import csv
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
POLICIES = ["idle", "uncontrolled", "edf", "edf-v2b", "llf", "llf-v2b"]
OUTPUT_FILES = ["slots.csv", "sessions.csv", "trace.csv", "summary.json"]


def sh(args, **kw):
    r = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        print(r.stdout)
        print(r.stderr, file=sys.stderr)
        raise SystemExit(f"command failed: {' '.join(map(str, args))}")
    return r.stdout


def sha_dir(d: Path) -> str:
    h = hashlib.sha256()
    for f in OUTPUT_FILES:
        h.update((d / f).read_bytes())
    return h.hexdigest()


def dr_covers(start: int, end: int, s: int) -> bool:
    return start < s <= end


def dr_stats(scenario_dir: Path, out_dir: Path):
    """Mean net load inside DR-covered slots (M30) and covered-slot count."""
    manifest = json.loads((scenario_dir / "scenario.json").read_text())
    if not manifest.get("dr_events_file"):
        return None
    events = list(csv.DictReader(open(scenario_dir / manifest["dr_events_file"])))
    covered = set()
    for e in events:
        s0, s1 = int(e["start_slot"]), int(e["end_slot"])
        covered.update(s for s in range(s0 + 1, s1 + 1))
    nets = [
        float(r["net_kw"])
        for r in csv.DictReader(open(out_dir / "slots.csv"))
        if int(r["slot"]) in covered
    ]
    return sum(nets) / len(nets) if nets else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", default=str(ROOT / "target" / "verification"))
    work = Path(ap.parse_args().workdir)
    work.mkdir(parents=True, exist_ok=True)

    print("== build ==")
    sh(["cargo", "build", "--release", "--quiet"])
    sh(["cargo", "run", "--release", "--quiet", "--bin", "gen_month"])
    binary = ROOT / "target" / "release" / "openv2b"

    # Persistence-off twin of the lossless month.
    nopersist = work / "one_month_nopersist"
    if nopersist.exists():
        shutil.rmtree(nopersist)
    shutil.copytree(ROOT / "examples" / "one_month", nopersist)
    manifest = json.loads((nopersist / "scenario.json").read_text())
    manifest["persistence"] = False
    (nopersist / "scenario.json").write_text(json.dumps(manifest, indent=2))

    datasets = {
        "one_month": ROOT / "examples" / "one_month",
        "one_month_lossy": ROOT / "examples" / "one_month_lossy",
        "one_month_nopersist": nopersist,
    }

    rows = []
    referee_failures = 0
    for ds_name, ds_dir in datasets.items():
        for policy in POLICIES:
            out_a = work / f"{ds_name}_{policy}_a"
            out_b = work / f"{ds_name}_{policy}_b"
            for out in (out_a, out_b):
                if out.exists():
                    shutil.rmtree(out)
                sh([binary, "--scenario", ds_dir, "--policy", policy, "--out", out])
            det = sha_dir(out_a) == sha_dir(out_b)
            ref = subprocess.run(
                [sys.executable, ROOT / "tools" / "referee.py", ds_dir, out_a],
                capture_output=True,
                text=True,
            )
            if ref.returncode != 0:
                referee_failures += 1
                print(ref.stdout[-3000:])
            summary = json.loads((out_a / "summary.json").read_text())
            bill = summary["bill"]
            rows.append(
                {
                    "dataset": ds_name,
                    "policy": policy,
                    "total": bill["total_usd"],
                    "energy": bill["energy_usd"],
                    "demand": bill["demand_usd"],
                    "penalty": bill["dr_penalty_usd"],
                    "incentive": bill["dr_incentive_usd"],
                    "peak_kw": bill["peak_net_kw"],
                    "dr_mean_net_kw": dr_stats(ds_dir, out_a),
                    "target_met": summary["sessions_target_met"],
                    "sessions": summary["sessions_total"],
                    "unserved": summary["sessions_never_connected"],
                    "missing_kwh": summary["missing_kwh"],
                    "banked_kwh": summary["banked_kwh"],
                    "clamped_kwh": summary["chain_clamped_kwh"],
                    "deterministic": det,
                    "referee": "PASS" if ref.returncode == 0 else "FAIL",
                    "sha": sha_dir(out_a)[:12],
                }
            )
            print(
                f"{ds_name:22s} {policy:13s} total=${bill['total_usd']:9.2f} "
                f"det={'ok' if det else 'FAIL'} referee={rows[-1]['referee']}"
            )

    # M31: peak reduction vs uncontrolled, M30 relaxation vs idle.
    report = ["# Month verification results", ""]
    for ds_name in datasets:
        sub = [r for r in rows if r["dataset"] == ds_name]
        unc = next(r for r in sub if r["policy"] == "uncontrolled")
        idle = next(r for r in sub if r["policy"] == "idle")
        report.append(f"## {ds_name}")
        report.append("")
        report.append(
            "| policy | total $ | energy $ | demand $ | DR penalty $ | DR incentive $ | peak kW "
            "| mean net in DR kW | peak red. vs unc. kW | targets met | unserved | missing kWh "
            "| banked kWh | clamped kWh | determinism | referee |"
        )
        report.append("|" + "---|" * 15)
        for r in sub:
            relax = "" if r["dr_mean_net_kw"] is None else f"{r['dr_mean_net_kw']:.2f}"
            report.append(
                f"| {r['policy']} | {r['total']:.2f} | {r['energy']:.2f} | {r['demand']:.2f} "
                f"| {r['penalty']:.2f} | {r['incentive']:.2f} | {r['peak_kw']:.1f} | {relax} "
                f"| {unc['peak_kw'] - r['peak_kw']:.1f} | {r['target_met']}/{r['sessions']} "
                f"| {r['unserved']} | {r['missing_kwh']:.2f} | {r['banked_kwh']:.2f} "
                f"| {r['clamped_kwh']:.2f} | {'ok' if r['deterministic'] else 'FAIL'} | {r['referee']} |"
            )
        report.append("")
        report.append(
            f"M30 building-load relaxation in DR windows vs idle ({idle['dr_mean_net_kw']:.2f} kW baseline): "
            + ", ".join(
                f"{r['policy']} {r['dr_mean_net_kw'] - idle['dr_mean_net_kw']:+.2f} kW"
                for r in sub
                if r["policy"] != "idle" and r["dr_mean_net_kw"] is not None
            )
        )
        report.append("")

    out_md = ROOT / "reports" / "month_results.md"
    out_md.parent.mkdir(exist_ok=True)
    out_md.write_text("\n".join(report))
    print(f"\nwrote {out_md}")

    bad_det = [r for r in rows if not r["deterministic"]]
    if referee_failures or bad_det:
        print(f"FAILURES: referee={referee_failures}, determinism={len(bad_det)}")
        return 1
    print("ALL RUNS PASS (referee + two-process determinism)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
