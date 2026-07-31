#!/usr/bin/env python3
"""One-command cross-simulator parity harness (reference format -> openv2b).

Usage:
  python3 tools/parity_optimus.py --test-episodes EP [EP ...]
      [--train-episodes EP ...] [--policies llf,oracle,mpc,scenario-mpc]
      [--reference-results DIR] [--reference-policy NAME]
      [--workdir DIR] [--binary PATH] [--no-build] [--demand-peak 11.67]

Reruns the whole comparison recorded in reports/BENCHMARK.md end to end:
converts reference-format episode directories with tools/convert_optimus.py,
runs the requested openv2b policies over the converted test episodes (the
training episodes become the `--futures` pool for `scenario-mpc`), and prints
bills, peaks, and wall-clock runtimes.

Reference results are OPTIONAL. Point `--reference-results` at a directory
tree in the reference simulator's own output layout (any depth, each run a
directory holding `summary_bldg_0.json` and `metrics.json`) and the harness
adds a per-episode delta table with attribution columns. Without it, or when
nothing matches, it says so and prints the openv2b table alone: the harness
must be runnable on a machine that has no reference data at all, so it is
deliberately NOT wired into CI.

Attribution columns (see docs/OPTIMUS_PORT.md for the full ledger):
  F-A  the reference's billing pipeline never bills the episode's FINAL
       15-minute interval. Computed exactly here from the openv2b run's last
       slot (imported energy x price), and expected to be the bulk of a
       positive delta: +$1.10 to +$1.14 per week-episode on RISHAV_WEEK.
  res  delta - F-A: what F-A does not explain. F-G (the reference discharging
       past its chargers' physical limits, -$0.34 to -$0.48/week) lives in
       here and cannot be computed from openv2b outputs alone; it needs the
       reference's own per-charger trace.

Recorded reference point (reports/BENCHMARK.md, RISHAV_WEEK/SEP2024 eps 1-3,
15 users, 768 slots, same machine):
  LLF     3883.71 / 3877.66 / 3930.38  ->  openv2b 3884.47 / 3878.38 / 3931.04
  oracle  3796.40 / 3752.26 / 3763.76  ->  openv2b 3797.28 / 3753.12 / 3764.90
  MPC K=5 3795.36 / 3773.39 / 3761.93  ->  openv2b 3796.95 / 3798.55 / 3763.33
  runtimes: LLF 0.7 s -> 1.2 ms; oracle ~1 s -> 47-60 ms; MPC K=5 ~360 s ->
  ~60 s (~6x, matched configuration).
"""

import argparse
import csv
import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONVERTER = ROOT / "tools" / "convert_optimus.py"
DEFAULT_POLICIES = "llf,oracle,mpc,scenario-mpc"
# Policies that need the in-process solver feature.
SOLVER_POLICIES = {"oracle", "mpc", "scenario-mpc"}
# Policies that need a pool of sampled futures.
FUTURE_POLICIES = {"scenario-mpc", "scenario-mpc-cplex"}


def sh(args, **kw):
    r = subprocess.run([str(a) for a in args], cwd=ROOT, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        print(r.stdout)
        print(r.stderr, file=sys.stderr)
        raise SystemExit(f"command failed: {' '.join(str(a) for a in args)}")
    return r.stdout


def table(header, rows):
    """Fixed-width text table (stdlib only, like the rest of tools/)."""
    widths = [max(len(str(h)), *(len(str(r[i])) for r in rows)) if rows else len(str(h))
              for i, h in enumerate(header)]
    line = "  ".join(str(h).ljust(w) for h, w in zip(header, widths))
    print(line)
    print("  ".join("-" * w for w in widths))
    for r in rows:
        print("  ".join(str(c).ljust(w) for c, w in zip(r, widths)))


# ------------------------------------------------------------- conversion


def convert(episode_dir: Path, out_dir: Path, demand_peak: float) -> Path:
    """Convert one reference-format episode; returns the scenario directory."""
    if out_dir.exists():
        shutil.rmtree(out_dir)
    sh([sys.executable, CONVERTER, episode_dir, out_dir, "--demand-peak", demand_peak])
    return out_dir


def episode_label(episode_dir: Path) -> str:
    """`<MONTHYEAR>/<id>` when the reference persistence layout is in use."""
    ep = episode_dir.resolve()
    month = ep.parent.name
    if month == "persistence":
        month = ep.parent.parent.name
    return f"{month}/{ep.name}" if month else ep.name


# ------------------------------------------------------------------ runs


def run_policy(binary: Path, scenario: Path, policy: str, out_dir: Path, futures):
    """Run one policy; returns (summary dict or None, seconds)."""
    if out_dir.exists():
        shutil.rmtree(out_dir)
    args = [binary, "--scenario", scenario, "--policy", policy, "--out", out_dir]
    if policy in FUTURE_POLICIES:
        args += ["--futures", ",".join(str(f) for f in futures)]
    start = time.perf_counter()
    r = subprocess.run([str(a) for a in args], cwd=ROOT, capture_output=True, text=True)
    seconds = time.perf_counter() - start
    if r.returncode != 0:
        print(f"  {policy}: run failed: {r.stderr.strip().splitlines()[-1:] or r.stdout.strip()}")
        return None, seconds
    return json.loads((out_dir / "summary.json").read_text()), seconds


def final_slot_energy_usd(scenario: Path, out_dir: Path) -> float:
    """F-A: the imported energy cost of the LAST slot, which the reference's
    billing pipeline never charges for."""
    dt = json.loads((scenario / "scenario.json").read_text())["slot_minutes"] / 60.0
    rows = list(csv.DictReader(open(out_dir / "slots.csv")))
    if not rows:
        return 0.0
    last = max(rows, key=lambda r: int(r["slot"]))
    return max(float(last["net_kw"]), 0.0) * dt * float(last["price_usd_per_kwh"])


# ------------------------------------------------------ reference results


def reference_runs(root: Path):
    """Every directory under `root` that looks like a reference run."""
    return sorted(p.parent for p in root.rglob("summary_bldg_0.json"))


def match_reference(candidates, episode_dir: Path, policy_filter):
    """Reference runs whose path mentions this episode's month and id."""
    ep = episode_dir.resolve()
    month = ep.parent.name
    if month == "persistence":
        month = ep.parent.parent.name
    hits = []
    for d in candidates:
        path = "/" + "/".join(part.lower() for part in d.parts) + "/"
        if month and month.lower() not in path:
            continue
        if f"/{ep.name.lower()}/" not in path and f"_{ep.name.lower()}/" not in path:
            continue
        if policy_filter and policy_filter.lower() != d.name.lower():
            continue
        hits.append(d)
    return hits


def read_reference(run_dir: Path):
    summary = json.loads((run_dir / "summary_bldg_0.json").read_text())
    seconds = None
    metrics_path = run_dir / "metrics.json"
    if metrics_path.exists():
        seconds = json.loads(metrics_path.read_text()).get("total_time")
    return summary, seconds


# ------------------------------------------------------------------ main


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--test-episodes", type=Path, nargs="+", required=True,
                    help="reference-format episode dirs to simulate")
    ap.add_argument("--train-episodes", type=Path, nargs="*", default=[],
                    help="reference-format episode dirs used as scenario-mpc futures "
                         "(must be disjoint from the test episodes)")
    ap.add_argument("--policies", default=DEFAULT_POLICIES)
    ap.add_argument("--reference-results", type=Path,
                    help="root of the reference simulator's results tree (optional)")
    ap.add_argument("--reference-policy",
                    help="only match reference runs whose policy directory has this name")
    ap.add_argument("--workdir", type=Path, default=ROOT / "target" / "parity")
    ap.add_argument("--binary", type=Path, help="prebuilt openv2b binary (skips the build)")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--demand-peak", type=float, default=11.67)
    args = ap.parse_args()

    policies = [p.strip() for p in args.policies.split(",") if p.strip()]
    if any(p in FUTURE_POLICIES for p in policies) and not args.train_episodes:
        print("scenario-mpc needs --train-episodes (its sampled futures)")
        return 1

    binary = args.binary
    if binary is None:
        binary = ROOT / "target" / "release" / "openv2b"
        if not args.no_build:
            feature = ["--features", "solver-highs"] if set(policies) & SOLVER_POLICIES else []
            print("== build ==")
            sh(["cargo", "build", "--release", "--quiet", *feature])
    if not Path(binary).exists():
        print(f"no openv2b binary at {binary}; build it or pass --binary")
        return 1

    work = args.workdir
    work.mkdir(parents=True, exist_ok=True)

    print("== convert ==")
    tests = {}  # label -> converted scenario dir
    sources = {}  # label -> the reference-format episode dir it came from
    for ep in args.test_episodes:
        label = episode_label(ep)
        tests[label] = convert(ep, work / "test" / label.replace("/", "_"), args.demand_peak)
        sources[label] = ep
    futures = [
        convert(ep, work / "train" / episode_label(ep).replace("/", "_"), args.demand_peak)
        for ep in args.train_episodes
    ]

    print("\n== openv2b ==")
    runs = {}  # (label, policy) -> (summary, seconds, out_dir)
    rows = []
    for label, scenario in tests.items():
        for policy in policies:
            out_dir = work / "out" / f"{label.replace('/', '_')}_{policy}"
            summary, seconds = run_policy(binary, scenario, policy, out_dir, futures)
            if summary is None:
                continue
            runs[(label, policy)] = (summary, seconds, out_dir)
            bill = summary["bill"]
            rows.append([
                label, policy,
                f"{bill['total_usd']:.2f}", f"{bill['energy_usd']:.2f}",
                f"{bill['demand_usd']:.2f}", f"{bill['dr_penalty_usd']:.2f}",
                f"{bill['dr_incentive_usd']:.2f}", f"{bill['peak_net_kw']:.2f}",
                f"{seconds:.3f}",
                f"{summary['sessions_target_met']}/{summary['sessions_total']}",
            ])
    if not rows:
        print("no openv2b run succeeded")
        return 1
    table(
        ["episode", "policy", "total $", "energy $", "demand $", "dr pen $",
         "dr inc $", "peak kW", "runtime s", "targets"],
        rows,
    )

    # ------------------------------------------------------- deltas
    print("\n== vs reference ==")
    if args.reference_results is None:
        print("no --reference-results given: skipping the delta table "
              "(openv2b numbers above stand on their own)")
        return 0
    if not args.reference_results.exists():
        print(f"--reference-results {args.reference_results} does not exist: skipping deltas")
        return 0
    candidates = reference_runs(args.reference_results)
    if not candidates:
        print(f"no summary_bldg_0.json under {args.reference_results}: skipping deltas")
        return 0

    if args.reference_policy is None and len(policies) > 1:
        print("note: no --reference-policy given, so every openv2b policy is compared "
              "against the same matched reference run (see the 'ref run' column)")
    delta_rows = []
    for (label, policy), (summary, seconds, out_dir) in runs.items():
        hits = match_reference(candidates, sources[label], args.reference_policy)
        if not hits:
            print(f"  {label}/{policy}: no reference run matched; skipped")
            continue
        if len(hits) > 1:
            print(f"  {label}/{policy}: {len(hits)} reference runs matched, using {hits[0]}")
        ref, ref_seconds = read_reference(hits[0])
        ref_total = ref.get("total_bill_usd")
        if ref_total is None:
            print(f"  {label}/{policy}: reference summary has no total_bill_usd; skipped")
            continue
        ours = summary["bill"]["total_usd"]
        f_a = final_slot_energy_usd(tests[label], out_dir)
        speedup = f"{ref_seconds / seconds:.1f}x" if ref_seconds and seconds > 0 else "-"
        delta_rows.append([
            label, policy, hits[0].name,
            f"{ref_total:.2f}", f"{ours:.2f}", f"{ours - ref_total:+.2f}",
            f"{f_a:+.2f}", f"{ours - ref_total - f_a:+.2f}",
            f"{ref.get('max_demand_pw_15min_kW', float('nan')):.2f}",
            f"{summary['bill']['peak_net_kw']:.2f}",
            f"{ref_seconds:.1f}" if ref_seconds else "-",
            f"{seconds:.3f}", speedup,
        ])
    if not delta_rows:
        print("no episode matched a reference run: nothing to compare")
        return 0
    table(
        ["episode", "policy", "ref run", "ref $", "openv2b $", "delta $",
         "F-A $", "res $", "ref peak kW", "peak kW", "ref s", "s", "speedup"],
        delta_rows,
    )
    print(
        "\nF-A = the final-slot interval the reference never bills (openv2b stays correct);\n"
        "res = delta - F-A, where F-G (reference over-limit discharge) and solver\n"
        "vertex choice live. Attribution rules: docs/OPTIMUS_PORT.md, reports/BENCHMARK.md."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
