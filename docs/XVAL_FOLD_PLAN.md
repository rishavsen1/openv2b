# Plan: folding the ACN-Sim cross-validation plugin into openv2b

Status: **proposal, not executed.** Nothing in this document has been applied. It is written to be
executed step by step with a verification command after each step.

Scope: move `~/acnportal-v2b` (Python, BSD-3-Clause, 8 commits, never pushed, no remote) into
`~/openv2b` (Rust, public, MIT OR Apache-2.0, DCO) as `xval/acnportal-v2b/`, correct the licensing
and provenance story, wire a non-intrusive CI trigger, and **replace the plugin's Python
reimplementations of openv2b's policies with a native translation layer**, so that the one true
policy implementation (Rust) drives and ACN-Sim serves purely as an independent physics engine.

Out of scope: rebuilding openv2b on ACN-Sim (never); publishing the plugin to PyPI; experiment X5
(billing parity); `scenario-mpc` replay (needs `--futures` fixtures).

**Revision note.** An earlier draft of this document framed the re-sync as a choice between
re-implementing openv2b's ported heuristics in Python ("Option A: refresh the mirrors") and
deleting three of them to shrink the claim ("Option B"). Both were wrong in the same way: a mirror
is a second copy of the algorithm that drifts, and deleting mirrors buys stability by discarding
coverage. This revision removes the concept entirely. Sections 3, 4 and 5 are new; sections 1, 2,
6 and 7 are updated in place. Section numbering changed: the translation layer is now section 3,
the independence claim is section 4, CI is section 5.

---

## 0. Facts established by inspection (the plan rests on these)

Read on 2026-07-31 at openv2b `a10703f` and plugin `dcc7a0c`.

### 0.1 Facts about the current (mirror-based) state

| # | Fact | Evidence |
|---|---|---|
| F1 | openv2b's `policy::by_name` accepts exactly seven names: `idle, uncontrolled, policy-0, policy-1, policy-2, edf, llf`. `edf-v2b` and `llf-v2b` **do not exist**. | `src/policy/mod.rs:23-48` |
| F2 | `src/main.rs` accepts six *further* names outside `POLICY_NAMES`: `mpc`, `mpc-cplex`, `oracle`, `oracle-cplex`, `scenario-mpc`, `scenario-mpc-cplex`. `mpc` and `oracle` are `#[cfg(feature = "solver-highs")]`; the `-cplex` variants need `OPENV2B_CPLEX_BIN`. | `src/main.rs:100-170` |
| F3 | An unknown policy name makes the binary print `unknown policy '<x>'` and return `ExitCode::FAILURE`. | `src/main.rs:167-173` |
| F4 | The plugin's runner maps five policy names, three of which are now unroutable: `uncontrolled, edf, edf-v2b, llf, llf-v2b`. | `src/acnportal_v2b/runner.py:23-30` |
| F5 | X3 runs `("edf-v2b", "llf-v2b", "llf")`; X4 runs `("edf-v2b", "llf-v2b", "uncontrolled")`. Under F3 + `subprocess.run(check=True)` those legs raise `CalledProcessError`, i.e. X3 and X4 **crash**, they do not report a diff. | `experiments/x3_v2b_discharge.py:45`, `experiments/x4_heterogeneous_ports.py:95`, `src/acnportal_v2b/openv2b.py` |
| F6 | openv2b's `Uncontrolled` was **not** touched by the port commit `a743277`. | `src/policy/heuristics.rs:44-68` |
| F7 | openv2b HEAD's charger assignment prefers a **bidirectional port for every car**, ties by lowest charger id, and **drops permanently** any car that finds no vacancy. | `src/engine.rs:167-193` |
| F8 | The plugin's replay instead matches port capability to vehicle capability and *retries* a waiting car on later slots. **Stale vs F7.** | `src/acnportal_v2b/scenario.py:250-271` |
| F9 | X4's fixture cannot detect F8: its bidirectional-capable vehicle (id 0) also arrives first, so both rules hand it charger 1. | `experiments/x4_heterogeneous_ports.py:59-62` |
| F10 | openv2b HEAD's `Vehicle` has `max_soc_kwh` (operating ceiling, distinct from `battery_kwh`, exposed as `Vehicle::ceiling_kwh()`) and the manifest has `heuristic_threshold_kw`. The plugin's scenario reader and its scenario *writer* know **neither**. | `src/scenario.rs:99-115`, `src/scenario.rs:35-39`; `src/acnportal_v2b/scenario.py:315-330`, `:514-545` |
| F11 | Missing CSV columns still deserialize (serde defaults), so the plugin's writer is format-compatible with HEAD even though it is semantically incomplete. | `src/scenario.rs` `#[serde(default)]` attributes; `examples/one_day/vehicles.csv` header has no `max_soc_kwh` |
| F12 | Both repos sign off every commit (DCO). | `git log --format='%(trailers:key=Signed-off-by)'` on both |
| F13 | openv2b's `Cargo.toml` has **no `exclude`/`include`**, so `cargo package` would ship every git-tracked file, including a folded `xval/`. | `Cargo.toml` |
| F14 | openv2b CI already requires `python3` (stdlib only) and already does `cargo build --release`. It does **not** build with `--features solver-highs`. | `.github/workflows/ci.yml` |
| F15 | The plugin's git history is 8 commits, 216 KiB of objects, ~3.8 kLOC, single author. | `git count-objects -vH`, `wc -l` |
| F16 | openv2b already contains `tools/referee.py`, a stdlib-Python re-simulation that runs on every PR. Its policy re-simulation covers exactly `idle, uncontrolled, policy-0, policy-1, policy-2, edf, llf`; its slot identities, trace reconciliation, DR settlement and bill recomputation are **policy-agnostic** and therefore also cover `mpc`/`oracle` runs. | `tools/referee.py:165-167`, `:722-723`, `:463-520`, `:660` |

### 0.2 Facts about the trace and the replay path (new; these carry section 3)

| # | Fact | Evidence |
|---|---|---|
| T1 | `TraceRecord` has exactly six fields: `slot`, `vehicle_id`, `arrival_slot`, `charger_id`, `power_kw` (**applied**, post-clamp, signed: positive grid-side charge, negative building-side discharge), `soc_kwh` (**end of slot**). | `src/engine.rs:33-44` |
| T2 | One trace row is emitted per **connected** session per slot, in canonical `(arrival_slot, vehicle_id)` order. A session that never obtained a charger produces **no rows at all**. | `src/engine.rs:327-340`, `:191` |
| T3 | `trace.csv` is written unconditionally by `output::write_results` whenever `--out` is given. **No new CLI flag is needed to obtain it.** | `src/output.rs:23-27`, `src/main.rs:181-187` |
| T4 | The engine's *requested* setpoint vector is built in the same scope as the trace emission, four statements earlier, and survives until the end of the slot body. Adding fields derived from it is local. | `src/engine.rs:272-278` vs `:327-340` |
| T5 | The engine filters out-of-range indices and non-finite powers **before** dedup, and dedup is `retain(!=idx)` then `push`, so a **later** setpoint for the same session both wins on value and takes the **later** emission position. A NaN emitted after a finite setpoint for the same session is discarded and the earlier finite value survives. | `src/engine.rs:272-278` |
| T6 | Both clamping passes iterate `&requested` in that order (charge pass skipping negatives, discharge pass skipping non-negatives), so a single per-session index into `requested` reproduces the rationing order of both passes. | `src/engine.rs:295-325` |
| T7 | `requested_kw` and `emission_index` are **not** recoverable from any current output. `slots.csv` and `sessions.csv` carry only realized quantities, and `trace.csv`'s row order is canonical, not emission order. | `src/engine.rs:17-44`, `src/output.rs` |
| T8 | The `csv` crate (1.4) serializes `f64` through `ryu`, i.e. the shortest decimal string that round-trips exactly. Rust `f64` -> `trace.csv` -> Python `float()` is **bit-exact**; a CSV replay introduces no numerical noise of its own. | `Cargo.lock` (`ryu 1.0.23`), csv 1.4 serializer |
| T9 | Nothing outside `src/engine.rs` constructs a `TraceRecord` literal; the three Rust test files that touch the trace only read fields. `tools/run_verification.py` SHA-256s output directories only to compare **two runs of the same binary**, so it is insensitive to added columns. No golden hash is pinned over `trace.csv`. | `rg 'TraceRecord'`; `tests/{property_sweep,review_regressions,mutation_kills}.rs`; `tools/run_verification.py:34,99` |
| T10 | `tests/audit_r2.rs::headroom_rationed_in_emission_order` (F1 of the R2 audit) pins in Rust that scarce headroom is rationed in the policy's emission order. | `tests/audit_r2.rs:13-57` |
| T11 | openv2b permits an arrival SoC anywhere in `[min_soc_kwh, battery_kwh]`, i.e. possibly **above** `max_soc_kwh`. In that case `room_kwh` is negative, `max_grid_kwh` is negative, and the charge clamp floors the applied power at 0 without lowering the SoC. | `src/engine.rs:396-404`, `docs/SPEC.md` section 6 validation list |
| T12 | The plugin's `BidirectionalBattery` rejects `init_charge > capacity` transitively through `Battery.__init__`, and `set_charge` requires `[min_charge, capacity]`. So mapping the ACN-Sim capacity onto openv2b's *ceiling* makes T11 an error case that must be handled explicitly. | `src/acnportal_v2b/models.py:150-166`, `:272-284` |
| T13 | `V2BChargingNetwork.update_pilots` clamps a pilot into `[min_rate, max_rate]` **before** choosing a pass by sign, whereas openv2b chooses the pass by the raw sign and clamps inside `apply_setpoint`. The two are numerically identical on every case (a negative request on a unidirectional port clamps to 0 and consumes no headroom on either side), but **pass membership differs**, so the harness must compare numbers, never pass membership. | `src/acnportal_v2b/network.py:196-241` vs `src/engine.rs:295-325` |
| T14 | ACN-Sim's per-EVSE unit basis is Amps times volts; the plugin pins 1000 V so 1 A == 1 kW exactly and no rounding enters. The arithmetic path from schedule to realized energy is therefore genuinely different from openv2b's kW-native path, even where the formulas agree. | `src/acnportal_v2b/models.py:12-18`, `scenario.py:57-58` |

### 0.3 Predicted behavior of the X-suite against HEAD, unchanged

To be confirmed in Step 0, not assumed.

| experiment | leg | prediction | why |
|---|---|---|---|
| X1 | `uncontrolled` x {eta 1.0, 0.92} | **green** | F6, F11; 1 EV : 1 charger so assignment is trivial |
| X2 | `uncontrolled` | **green** | all six ports unidirectional, so F7 and F8 agree |
| X2 | `edf` | **red, large** | HEAD `edf` is a threshold-budget scheduler; with no `heuristic_threshold_kw` the fallback threshold is `0.8 x 18 = 14.4` kW, which the 18 kW midday building load already exceeds |
| X3 | `edf-v2b`, `llf-v2b` | **crash** | F3 + F5 |
| X3 | `llf` | **red** | same class as X2/`edf` |
| X4 | `edf-v2b`, `llf-v2b` | **crash** | F3 + F5 |
| X4 | `uncontrolled` | **green (vacuously)** | F9 |

The baseline exists to confirm the diagnosis before any commit, and to give a "before" number for
the claim rewrite. Under the new design every one of these legs is replaced, so a surprise here
changes the abort decision (A1) but not the architecture.

---

## 1. Target layout and history strategy

### 1.1 Layout

```
openv2b/
  xval/
    README.md                      <- new: what xval/ is, why it is not Rust, how to ignore it
    acnportal-v2b/                 <- the plugin, self-contained
      LICENSE                      (BSD-3-Clause, unchanged, verbatim)
      README.md                    (rewritten header + corrected claims)
      KNOWN_ISSUES.md              (rewritten: items 1 and 4 are resolved by the fold)
      DERIVATION.md                <- new: for each piece of plugin code that re-implements an
                                      openv2b behavior, the prose source it was written from
      RESULTS.md                   <- new: the parity table, anchored to an openv2b commit, the
                                      binary sha256, the acnportal version, a CI run URL, the tier
                                      (section 4.2) each row belongs to, and which MODE produced
                                      each number (free-running numbers carry the claim; anchored
                                      numbers are diagnostic and must be labelled as such)
      TRACE_CONTRACT.txt           <- new: the trace.csv columns the replay layer requires
      REPLAYED_POLICIES.txt        <- new: the openv2b policy names whose traces the suite replays
      pyproject.toml, requirements.txt, requirements.lock.txt
      .gitignore                   (kept: scopes .venv/, __pycache__/, xval_runs/ locally)
      src/acnportal_v2b/*.py
      experiments/*.py
      tests/*.py
```

`MIRRORED_POLICIES.txt` from the earlier draft is gone: there are no mirrors to enumerate.
`TRACE_CONTRACT.txt` replaces it as the machine-checked tripwire, and it guards a stronger thing
(a data contract, mechanically parseable out of `src/engine.rs`) than a list of policy names ever
could.

Why two levels rather than putting the package directly in `xval/`:

- the Python package root stays self-contained, so `xval/acnportal-v2b/LICENSE` unambiguously
  governs a directory whose boundary is obvious, and
  `pip install "git+https://github.com/rishavsen1/openv2b#subdirectory=xval/acnportal-v2b"` works
  unchanged, and a later PyPI publish is a `working-directory:` away;
- `xval/` is left free for a second cross-validator (`xval/acndata/`, a future SAA reference)
  without reshuffling.

`xval/` is **not** a cargo workspace member. openv2b's `Cargo.toml` declares a single package with
no `[workspace] members`, and cargo auto-discovers targets only under `src/bin`, `tests`,
`examples`, `benches` at the package root. Nothing in `xval/` is compiled, linted, or formatted by
cargo.

### 1.2 Two pull requests, not one

The trace-schema extension (section 3.5) is a change to openv2b's core output. It must land
**before and separately from** the import, as **PR0**, for three reasons:

1. It is justifiable on openv2b's own terms without reference to any plugin: `trace.csv` today
   records what the engine *applied* but not what the policy *asked for*, so no external checker
   (including `tools/referee.py`) can distinguish "the policy asked for less" from "the engine
   clamped". That is a genuine hole in a file whose stated purpose (SPEC section 6) is external
   verification.
2. It keeps the "no reverse coupling" story clean: the field exists because the output format was
   incomplete, not because a Python package wanted it. Nothing in openv2b's source, comments, or
   tests will name acnportal.
3. It keeps PR1 reviewable as what it is: an import plus hygiene.

**PR0**: SPEC section 3 and 6 wording, the two `TraceRecord` fields, one Rust test, optionally one
new policy-agnostic check in `tools/referee.py`.
**PR1**: the fold (import, licensing, `exclude`, `check_xval_sync.py`, `xval.yml`, the translation
layer, the rewritten experiments, the claim rewrite).

### 1.3 History: recommendation

**Recommended: Option A, path-rewritten history merge.** Clone the plugin to a throwaway location,
rewrite every historical path under `xval/acnportal-v2b/`, then merge that rewritten history into
an openv2b branch with `--allow-unrelated-histories`.

Rationale:

- The 8 commits are evidence for the surviving part of the independence claim (section 4): they
  show the plugin's model classes, network guard and interface were written before any parity run,
  against `docs/SPEC.md`, in a repo that never contained a line of Rust. A squashed import throws
  that away and replaces it with an assertion. This matters *more* under the new design, not less:
  the site-level guard (`network.py`) is now the single largest piece of same-author,
  spec-derived code in the comparison, so its provenance is exactly what a reviewer will probe.
- All 8 commits are DCO-signed by the same person who owns openv2b (F12), so the sign-off chain
  carries over intact and no relicensing consent problem arises.
- Path rewriting (rather than plain `git subtree add`) is what makes the history *usable*: after a
  subtree add the historical commits still record paths like `src/acnportal_v2b/models.py`, so
  `git log -- xval/acnportal-v2b/` shows only the merge and `git blame` stops at it.

Honest caveats to record in the import commit message:

- Rewriting changes every commit hash. "History preserved" means messages, authorship, dates,
  sign-offs and diffs are preserved; the 8 original SHAs are not. Record them in
  `xval/acnportal-v2b/DERIVATION.md` and keep an off-repo `git bundle` of the pre-import repo.
- `git filter-repo` is an external tool (`pipx install git-filter-repo`). Fallbacks are below.

**Rejected: Option B, `git subtree add`.** Preserves commits but not paths. Use only if
filter-repo cannot be installed and the owner still wants the commits.

**Rejected: Option C, fresh squashed import.** Cheapest, but discards the provenance evidence and
the per-commit sign-offs for no benefit given F12. Keep it as the abort-path fallback only.

### 1.4 Exact commands (Option A)

```bash
# --- Safety rails. Never run filter-repo inside either real repository. ---
git -C /home/rishav/openv2b       status --porcelain     # must be empty
git -C /home/rishav/acnportal-v2b status --porcelain     # must be empty
git -C /home/rishav/acnportal-v2b bundle create ~/acnportal-v2b-preimport.bundle --all
git -C /home/rishav/acnportal-v2b log --format='%H %ad %s' --date=iso > ~/acnportal-v2b-preimport-hashes.txt

# --- 1. Rewrite a throwaway clone so history carries the destination paths. ---
git clone --no-local /home/rishav/acnportal-v2b /tmp/xval-import
cd /tmp/xval-import
git filter-repo --to-subdirectory-filter xval/acnportal-v2b
git log --oneline --stat | head -20          # every path must read xval/acnportal-v2b/...
git branch --show-current                    # note the branch name (expected: main)

# --- 2. Import onto an openv2b branch. ---
cd /home/rishav/openv2b
git switch -c xval-fold
git remote add xval-import /tmp/xval-import
git fetch xval-import
git merge --allow-unrelated-histories --no-ff --no-commit xval-import/main
git commit -s        # message: see 1.5
git remote remove xval-import

# --- 3. Everything else (hygiene, docs, CI, translation layer) as ordinary signed commits.
```

Fallback B (no filter-repo, commits kept, paths not rewritten):

```bash
cd /home/rishav/openv2b && git switch -c xval-fold
git remote add xval-import /home/rishav/acnportal-v2b
git fetch xval-import
git subtree add --prefix=xval/acnportal-v2b xval-import main
git remote remove xval-import
```

Fallback C (fresh import; `git archive` ships **tracked files only**, so `.venv/`, `__pycache__/`,
`.pytest_cache/`, `*.egg-info/` and `xval_runs/` cannot leak in, whereas a plain `cp -r` would leak
all five):

```bash
cd /home/rishav/openv2b && git switch -c xval-fold
mkdir -p xval/acnportal-v2b
git -C /home/rishav/acnportal-v2b archive main | tar -x -C xval/acnportal-v2b
git add xval && git commit -s
```

### 1.5 Merge-strategy requirement (easy to get wrong)

If PR1 lands through GitHub it **must be merged with "Create a merge commit"**. "Squash and merge"
collapses the imported history into one commit and silently defeats Option A; "Rebase and merge"
refuses or flattens a merge commit. If the repository's default merge method is squash, either
change it for this PR or push `xval-fold` to `main` directly.

Import commit message skeleton:

```
xval: fold the acnportal-v2b cross-validation plugin into xval/acnportal-v2b

Imports the ACN-Sim V2B plugin (8 commits, BSD-3-Clause, previously the
standalone local repo ~/acnportal-v2b, never published) with its history
rewritten under xval/acnportal-v2b/. openv2b remains standalone: nothing in
src/ imports, links, or depends on anything under xval/.

The plugin keeps its own BSD-3-Clause LICENSE; see README.md "Provenance and
license" and docs/PROVENANCE.md for the per-directory licensing rule, and
Cargo.toml's `exclude` for why the published crate contains no BSD-3 files.

Original commit ids (pre-rewrite): 8a423a4, e189bce, 2c3fee8, 5a019e9,
6a970af, 94c98c3, 532d8b2, dcc7a0c.
```

---

## 2. Licensing hygiene

The plugin is BSD-3-Clause **by its own choice**, for attribution symmetry with acnportal. It is
not a derivative work of acnportal in the copyright sense: no acnportal source is copied or
patched; acnportal is a pip dependency resolved at runtime. That distinction must be stated
precisely, because "BSD because acnportal is BSD" is a claim about obligation, and it is false
here. Mixing permissive licenses is trivially legal, so the only real risk is *misdescription*.

### 2.1 What each artifact must say

| Artifact | Required content |
|---|---|
| `xval/acnportal-v2b/LICENSE` | Unchanged, verbatim BSD-3-Clause with the existing acnportal attribution preamble. Never edited, never relicensed. |
| `xval/acnportal-v2b/README.md` | First section after the title: "**License: BSD-3-Clause** (see `LICENSE`), unlike the rest of this repository, which is MIT OR Apache-2.0. This directory is not compiled into, linked by, or packaged with the `openv2b` crate." |
| `xval/README.md` (new) | One screen: what `xval/` is for, that it is Python and optional, that Rust-only contributors need none of it, and the per-directory license rule. |
| Root `README.md`, "Provenance and license" | Add: "Everything in this repository is `MIT OR Apache-2.0` **except `xval/acnportal-v2b/`**, which is BSD-3-Clause (its own `LICENSE`). The published `openv2b` crate excludes `xval/` entirely." |
| Root `LICENSE-MIT`, `LICENSE-APACHE` | **Do not edit.** They are license texts, not scope statements. Scope belongs in README/PROVENANCE. Editing a canonical license text is itself a licensing defect. |
| `CONTRIBUTING.md` | Amend "All contributions are accepted under `MIT OR Apache-2.0`" to except `xval/acnportal-v2b/`, whose contributions are accepted under BSD-3-Clause. Without this the DCO sign-off on future xval commits certifies the wrong license. |
| `docs/PROVENANCE.md` | Section 1 item 3 currently says "No code has been vendored from them to date". Still true but now misleading. Add "Third-party-licensed subdirectories" naming `xval/acnportal-v2b/`, its license, why (attribution symmetry, not obligation), and the rule that BSD-3 content must never move into `src/`, `tests/`, `examples/`, or `tools/`. |
| Every `.py` file under `xval/acnportal-v2b/` | Add `# SPDX-License-Identifier: BSD-3-Clause` as the first line. This is cheap insurance: if a file is ever copied out of the directory, its license travels with it. |

### 2.2 Cargo packaging

F13 is a real defect once the fold lands: `cargo package` collects git-tracked files, so the
`.crate` would contain BSD-3-Clause Python under a manifest declaring `license = "MIT OR
Apache-2.0"`. That is a misrepresentation on crates.io, and it bloats the crate with files no
consumer can use.

Fix, in `[package]`:

```toml
exclude = ["/xval"]
```

Use `exclude`, not `include`: `include` is a whitelist and would silently drop `examples/`,
`docs/`, and `tools/` (which `tools/referee.py` consumers and the docs links depend on) the next
time someone adds a directory.

Notes:

- crates.io status: the manifest is still `version = "0.1.0"` while the project calls itself
  v0.4-alpha, which strongly suggests nothing has been published. **Confirm before publishing**
  (`cargo search openv2b`): if 0.1.0 *is* already published it cannot be replaced, and the exclude
  only takes effect from the next version.
- The exclusion must be verified **with a negative control**; see Step 10 in section 6.

### 2.3 Cosmetic and tooling side effects

- **GitHub Linguist** will start reporting a large Python share. Add `.gitattributes`:
  `xval/** linguist-detectable=false`. Prefer `linguist-detectable=false` over
  `linguist-vendored`: the code is not vendored and marking it so would be a small untruth in
  exactly the document (provenance) where untruths are expensive.
- **Dependency graph / Dependabot**: a public repo's dependency graph will ingest
  `requirements.txt` and permanently flag `numpy<2`, `pandas<2`, `setuptools<81`. These pins are
  deliberate (acnportal 0.3.3 predates numpy 2 and imports `pkg_resources`). Either leave
  Dependabot security updates off, or add `.github/dependabot.yml` ignoring the three packages with
  the reason in a comment. Do not "fix" the pins; that breaks the plugin.
- **GitHub license detection** reads root-level license files only; the nested `LICENSE` should not
  change the repository's detected license. Verify after push (section 6, Step 20).

---

## 3. The translation layer (this replaces the mirror re-sync)

### 3.1 Why the mirrors go

The plugin currently contains `algorithms.py`: Python reimplementations of `Uncontrolled`, `EDF`
and `LLF`, plus a `v2b=True` discharge overlay that corresponds to no openv2b policy at all. Three
of the five names it exports are unroutable at HEAD (F4, F5). That is drift, and it is not the
avoidable kind:

- Every future change to a ported policy re-breaks a mirror. The tripwire proposed in the earlier
  draft (a policy-name set check) catches renames and deletions but is blind to a semantic change
  under an unchanged name, which is half of what commit `a743277` did.
- The shortest path from a red mirror to a green one is to open `src/policy/heuristics.rs` and
  transcribe. The moment anyone does that, agreement stops being evidence of anything.
- Mirrors cap the reachable coverage at "policies someone is willing to re-write in Python". That
  permanently excludes `mpc`, `oracle` and `scenario-mpc`, which is where the interesting physics
  (deep V2B, floor-binding discharge, no-export saturation) actually lives.

The replacement principle: **the plugin contains no translated policy code.** The Rust
implementation is the only implementation. ACN-Sim is used as an independent physics engine
downstream of it.

There are two credible mechanisms for that. They are evaluated below on what each proves, what it
cannot prove, what has to be built on each side, its failure modes, and its cost.

### 3.2 Mechanism A: setpoint replay (open loop)

Run openv2b normally. It already emits, per slot and per connected session, the applied power and
the end-of-slot SoC (T1). Feed the corresponding **requested** setpoint sequence into ACN-Sim
through the plugin's bidirectional EVSE layer and compare the resulting per-session powers, SoC
trajectories, per-session energies and aggregate site load.

**What it proves.** Given an identical session set, identical port and vehicle limits, identical
entry SoC, identical requested setpoints, identical emission order, and identical building
load/site cap, the two engines produce the same applied power per session per slot, the same
end-of-slot SoC, the same per-session drawn/exported energy, and the same aggregate net load. That
is: the clamping cascade (port limit, vehicle limit, battery room, SoC floor, site cap, no-export
guard), the efficiency split, the SoC recursion, the energy accounting, and the aggregation are
reproduced by a differently-structured program running inside a third-party event loop with a
different unit basis (T14). It proves this for **any** openv2b policy, including ones nobody would
ever re-implement.

**What it cannot prove.** Everything upstream of the requested vector is *self-reported by the
engine under test* and is therefore outside the comparison boundary:

1. the policy logic (by construction, and deliberately);
2. the setpoint filter and last-write-wins dedup (T5);
3. the emission order itself (T6): the plugin is *told* the order, so a bug that reorders emission
   would be replayed faithfully and agree;
4. charger assignment (T2's `charger_id` is consumed as an input, not re-derived).

It also cannot prove anything about billing (nothing in the ACN-Sim leg computes a bill), nor about
receding-horizon planning quality.

The governing rule that keeps this from becoming circular: **openv2b may export anything that is an
*input* to the compared computation; exporting an *output* of that computation and feeding it back
in is circular and forbidden.** `requested_kw`, `emission_index` and `charger_id` are inputs.
`power_kw` and `soc_kwh` are outputs and may only be used as comparison targets, with one declared
exception (the anchored mode below), which is why the anchored mode must never be the mode that
carries the claim.

**Failure mode: trajectory separation.** If the engines disagree at slot `s`, the SoCs diverge, and
from `s+1` onward the replayed requests were computed by openv2b from a state ACN-Sim no longer
shares. Every later comparison is apples to oranges. This is real and it must be designed for, not
tolerated. The design:

- **Free-running mode** (the mode that carries the claim). No state is injected after
  initialization. The harness computes the **first divergence slot** across all compared
  quantities. Everything at or after that slot is labelled "downstream of divergence" and is
  explicitly *not* counted as evidence, in the printed table and in `RESULTS.md`. A run is green
  only if there is no divergence slot at all, so for a green run the distinction never bites; it
  exists so a red run reports one location instead of a wall of derived noise.
- **Anchored mode** (diagnosis). At the start of every slot the harness overwrites each connected
  session's ACN-Sim SoC with openv2b's end-of-previous-slot `soc_kwh` (or, for the session's first
  slot, its `soc_arrival_kwh` from `sessions.csv`). Each slot then becomes an independent
  single-step differential test: identical entry state plus identical requests, so do the two
  engines produce the same applied power and the same exit SoC? This yields the complete map of
  disagreements rather than only the earliest one.
- **Cross-check between the modes** (this is what stops either from being vacuous). The harness
  asserts that the set of slots with a non-zero anchoring correction is empty **iff** the
  free-running run is clean, and that when both are dirty they report the same first divergence
  slot. A disagreement between the two modes means the harness itself is broken, and it is reported
  as a harness failure rather than a parity failure.
- Under anchoring, cumulative per-session energy counters accumulate on top of injected states, so
  end-of-session totals are only compared in free-running mode. Anchored mode compares per-slot
  deltas.

**Second failure mode: vacuity.** If every replayed request is already feasible in ACN-Sim, nothing
clamps and agreement is trivial. Section 3.8 makes this a machine-checked property rather than an
argument.

**What has to be built.**

- *Rust side*: two new `TraceRecord` fields (section 3.5), roughly fifteen lines, plus SPEC wording
  and one test. No new binary, no new mode, no new flag (T3).
- *Python side*: a trace reader; a `TraceReplayAlgorithm` that returns the recorded request per
  station and sets the clamp order from `emission_index`; a rewritten bridge that takes charger
  assignment from the trace instead of re-deriving it; `max_soc_kwh` support; dropped-session
  handling; the anchoring hook; and the extended comparison. Against that, `algorithms.py`
  (242 lines) and the assignment replay (~100 lines) are deleted.

**Effort.** PR0: half a day including the SPEC edit and the test. PR1's translation layer: 1.5 to 2
days including fixtures, the deleted-code fallout in `tests/`, and the mutation pass.

### 3.3 Mechanism B: closed-loop co-simulation

ACN-Sim steps; at each period it asks the Rust side for setpoints over a line protocol (stdin/stdout
or a socket); both engines run the same controller in the loop. Requires a new openv2b mode that
reads state and writes setpoints.

**What it proves.** That the Rust controller can be driven by a foreign environment and produces a
sensible trajectory there. That is a genuine *portability* result and it is the right artifact if
the goal is "our scheduler runs on ACN-Sim".

**What it cannot prove, and this is decisive: it has no oracle.** Under (B) the controller responds
to ACN-Sim's state, so (B)'s trajectory and openv2b's native trajectory differ *by construction*
whenever the engines differ at all. A non-zero difference is therefore uninterpretable: it could be
an engine bug or the controller correctly reacting to a legitimately different state. To interpret
it you have to find the first slot where the two states diverge and compare the one-step response,
which is exactly what mechanism A gives you directly and cheaply. When the engines agree, (B)
returns the same verdict as (A) and adds nothing.

The residual unique yield of (B) over (A) is the class "openv2b's observation construction is
inconsistent with its own engine state" (the policy sees a stale or wrong SoC), because (A) takes
the request as given. Two things shrink that to near zero here:

- The observation's `soc_kwh` is read directly off the live session object four statements before
  `policy.decide` is called (`src/engine.rs:219-240`, `:273`), so the inconsistency is not
  expressible in the current code shape without a deliberate rewrite.
- For the seven built-in heuristics, `tools/referee.py` builds its own observation and re-simulates
  (F16), so an observation bug shows up there.

**What has to be built.** A new openv2b entry point that loads the scenario, holds one policy
instance per run, receives per-slot dynamic state (slot index, per-session `vehicle_id`,
`arrival_slot`, `soc_kwh`, effective directional limits, plus building load, price, TOU, cap, DR
firm level), constructs an `Observation`, calls `decide`, and writes setpoints back. Plus protocol
versioning, timeouts, deadlock handling, and a Python client. Plus a decision about float encoding:
a text protocol at less than 17 significant digits injects a divergence source that the comparison
then blames on the engine (`trace.csv` avoids this for free, T8, but a hand-rolled protocol does
not).

**The decisive structural objection.** A server mode constructs the observation a second time.
Unless it is refactored to share one constructor with `engine::run`, it is a second, drift-prone
copy of exactly the thing this whole revision exists to eliminate, this time in Rust where the
per-PR Python tripwire cannot see it. Rebuilding the mirror problem inside openv2b to solve the
mirror problem in the plugin is not progress.

**Effort.** 4 to 7 days, plus permanent maintenance of a second entry point that must stay
semantically identical to the engine's observation construction. The solver-feature problem
(section 5.3) applies to it as well.

### 3.4 Comparison and recommendation

| | A: setpoint replay | B: closed-loop co-simulation |
|---|---|---|
| Policy code in Python | none | none |
| Works for `mpc` / `oracle` | yes (policy-agnostic) | yes, but needs the solver inside the server |
| Has an oracle for disagreement | **yes**: identical inputs, so any difference is a bug | **no**: divergence is expected and uninterpretable |
| Tests the clamping cascade | yes, and it is the whole point | yes, but against no reference |
| Tests emission order | no (told) | no (the server decides it) |
| Tests dedup / filter | no (self-reported) | no |
| Tests charger assignment | no (told) | no (the server has no assignment stage) |
| New drift surface created | a data contract (mechanically checkable) | a second observation constructor in Rust |
| openv2b changes | 2 output columns | a new entry point + protocol |
| Effort | ~2 days | ~5 days + maintenance |
| Numerical fidelity of the interface | bit-exact (T8) | needs deliberate 17-digit or hex-float encoding |

**Recommendation: mechanism A, in both free-running and anchored modes. Do not stage B.**

The staged-path framing ("A then B") is tempting and should be resisted, because B is not a
strictly stronger A. It is a different artifact with a different purpose, and as *verification* it
is weaker than A at three times the cost, because it has no oracle. Committing to it now would put
a large item on the roadmap whose payoff cannot be stated.

**Does declining to stage B leave a verification gap? No.** Compare the tier-3 list of section 4.2
against what B would cover: B does not cross-validate the policies (it runs the same one), the
setpoint filter/dedup (the server applies openv2b's, or a copy of it), the emission order (the
server produces it), charger assignment (the server has no assignment stage at all, because
assignment happens inside `engine::run`, which B bypasses), or billing. B therefore covers *less*
of tier 3 than A does, not more. The only class it adds is observation-construction consistency,
which section 3.3 shows is not expressible in the current code shape and is covered by `referee.py`
for the seven heuristics. Declining to stage B costs a portability demonstration, not coverage, and
that is how the roadmap must describe it.

Instead, record B as a **conditional** future item with an explicit trigger and an explicit
relabel:

- **Relabel.** If it is ever built, it ships as a *portability demonstration*
  (`xval/acnportal-v2b/experiments/demo_closed_loop.py`), not as a cross-validation experiment, and
  `RESULTS.md` must not quote a delta from it as parity evidence.
- **Trigger.** Build it only when a policy is added whose decision depends on realized state in a
  way that mechanism A cannot exercise, i.e. a policy whose *request* is not a function of the
  observation alone (external I/O, wall clock, adaptive state that survives across runs). No
  current or planned policy is in that class; `Policy::decide` is a deterministic function of the
  observation plus per-episode instance state by contract (`src/policy/mod.rs:11-20`).
- **Precondition.** If built, the server mode must call a shared observation constructor factored
  out of `engine::run`, not a second copy. That refactor is a prerequisite, not a follow-up.

### 3.5 Is the trace format sufficient for A as it stands? No. Here is exactly what is missing.

**What the replay layer needs, per slot, and where it comes from:**

| Need | Source today | Sufficient? |
|---|---|---|
| Which sessions are connected this slot | `trace.csv` rows (T2) | yes |
| Which charger each connected session holds | `trace.csv` `charger_id` | yes |
| Port power limit and bidirectional flag | `chargers.csv` joined on `charger_id` | yes |
| Vehicle-side limits, capacity, floor, target | `vehicles.csv` | yes |
| Operating ceiling `max_soc_kwh` | `vehicles.csv` column exists (F10) | **format yes, plugin no**: the reader ignores it and the writer never emits it. Bridge fix, not a format change |
| Effective (persistence-chained) arrival SoC | `sessions.csv` `soc_arrival_kwh` | yes, but only used in **anchored** mode; free-running mode must derive it itself so SPEC invariant 11 stays cross-validated. Note the conditionality precisely: the chain is re-derived by the plugin, but the *set of dropped sessions* it chains through is read from openv2b (tier 3), so the free-running chain claim is "correct given the drop set", not "correct including the drop set" |
| Which sessions never connected | `sessions.csv` `never_connected` + absence of trace rows | yes, and the two must be asserted consistent |
| Building load, price, TOU, cap, efficiencies, DR | scenario directory | yes |
| **The requested (pre-clamp) signed setpoint per session per slot** | nowhere (T7) | **NO. New field required.** |
| **The emission order within the slot** | nowhere: trace rows are in canonical order, not emission order (T2, T7) | **NO. New field required.** |
| Applied power and end-of-slot SoC, for comparison | `trace.csv` `power_kw`, `soc_kwh` | yes |

Replaying the *applied* power instead of the requested power is not an acceptable fallback. Applied
power is post-clamp and therefore feasible by construction, so ACN-Sim's clamps would never bind
and the comparison would degenerate into "does a battery integrate a feasible power correctly",
which tests almost nothing and would nonetheless be reported as green. The requested field is what
gives the experiment content.

**PR0, precisely.** Add two fields to `src/engine::TraceRecord`, emitted in `trace.csv` as two new
columns:

```rust
/// The signed power the policy asked for this session this slot, kW, before
/// any clamping: same sign convention as `power_kw` (positive grid-side
/// charge, negative building-side discharge). Recorded AFTER the engine
/// discards out-of-range session indices and non-finite powers and AFTER
/// last-write-wins deduplication, i.e. this is the setpoint that actually
/// entered the clamping passes. A session the policy did not name this slot
/// records 0.0. `power_kw - requested_kw` is exactly what the engine clamped.
pub requested_kw: f64,

/// This session's 0-based position in the engine's deduplicated request
/// vector for this slot, which is the order in which site-cap and no-export
/// headroom is rationed. -1 for a session the policy did not name (it enters
/// neither pass and consumes no headroom).
pub emission_index: i64,
```

Field name, unit and semantics are fixed by the above and mirrored verbatim into
`xval/acnportal-v2b/TRACE_CONTRACT.txt`.

Implementation, entirely inside the existing slot body (T4): after the `requested` vector is built
and before the trace loop, materialize a per-view array initialized to `(0.0, -1)` and fill it by
enumerating `requested`. Enumerating the *final* vector (rather than recording at emission time) is
what makes T5's subtleties come out right for free: an out-of-range index never appears, a NaN
emitted after a finite setpoint for the same session leaves the earlier finite value in place, and
a superseded setpoint reports its *later* position.

Three consequences worth stating rather than discovering:

1. **This answers, mechanically, the open question the earlier draft flagged.** That draft asked
   which position a de-duplicated session takes in the rationing order and proposed to settle it by
   reading the engine and writing the answer into a document. It is now emitted in the data (T5,
   T6), so it cannot be recorded wrongly and then drift.
2. **No CLI flag, and the columns are unconditional.** A flag would create a schema variant, and a
   plugin that silently fell back to applied-power replay when the columns were absent would
   produce the vacuous green described above. Instead the columns always exist, and the plugin
   **hard-errors** (not skips, not warns) on a trace that lacks them. A binary too old for the
   replay layer must fail loudly.
3. **Nothing downstream breaks** (T9): no literal `TraceRecord` construction exists outside
   `engine.rs`, the three Rust tests that read the trace access fields by name, `csv::Reader` in
   both `tools/referee.py` and the plugin is header-keyed, and `run_verification.py`'s SHA-256 is a
   run-to-run comparison. The historical shas quoted in `reports/OVERNIGHT_REPORT.md` will no
   longer reproduce; that file is a dated report, not a gate, and PR0 should say so in its message.

**Optional, and recommended in PR0 because it is three lines**: give `tools/referee.py` a new
policy-agnostic check, `M-clamp`, asserting `|power_kw| <= |requested_kw| + tol` and
`sign(power_kw) in {0, sign(requested_kw)}` for every trace row. This extends referee coverage to
`mpc`/`oracle` runs, and it is a second, in-repo consumer of the new columns, so their existence
does not rest on the plugin.

**Explicitly *not* added now, but specified so the boundary can be widened later without a
redesign.** The filter and dedup rules (T5) and the emission order (T6) stay outside the comparison
boundary under the two-column design. Bringing them in requires the *raw* policy output, which does
not fit one row per (slot, session). If that is ever wanted, the shape is a separate
`decisions.csv` with columns `slot, emission_seq, session_index, vehicle_id, arrival_slot,
power_kw`, one row per raw emission in emission order, written before any filtering; the plugin
would then apply the range filter, the finite filter and last-write-wins itself, from SPEC prose,
and `emission_index` would become redundant. That is roughly six lines of bookkeeping in the
plugin, which is a different order of thing from a 250-line policy mirror, but it is still
plugin-side re-implementation, and it buys coverage of a rule that `tests/mutation_kills.rs` and
SPEC invariant 9 already pin in Rust and that `referee.py` re-derives for the seven heuristics
(F16). Deferred deliberately, with the cost of deferral named in section 4.3.

### 3.6 What has to be built on the Python side

Deletions (this is most of the change):

| Delete | Lines | Why |
|---|---|---|
| `algorithms.py`: `Uncontrolled`, `PrioritizedV2BAlgorithm`, `EDF`, `LLF`, `_vid` | ~200 | policy mirrors |
| `scenario.py::_replay_charger_assignment` | ~100 | re-derives an engine rule that the trace now reports |
| `interface.py`: `V2BSessionInfo.laxity_slots`, `.discharge_budget_kwh`, `.remaining_need_kwh` | ~40 | policy-decision helpers; they exist only to serve the mirrors and they encode openv2b's algorithm semantics |
| `interface.py`: `V2BInterface.headroom_kw` | ~15 | borderline, resolved toward deletion: it combines the site cap with the DR firm level the way openv2b's *policies* do, not the way its *engine* does (the engine's charge headroom is cap-only; the DR level never enters it, `src/engine.rs:291-293`). That makes it policy semantics wearing plumbing's clothes |
| `runner.POLICIES` | 7 | there is nothing to map |

Kept deliberately, with reasons, because "delete everything policy-shaped" would over-shoot:

- `V2BInterface`'s signal plumbing (`building_load`, `tou_class`, `dr_events`,
  `active_dr_firm_level`, prices, demand rates). Inert under replay, but it is the substrate a
  future X5 (billing parity) needs, and it encodes no algorithm. (`headroom_kw` is the one
  borderline member of that group and it goes with the mirrors instead; see the deletion table for
  why.)
- `V2BChargingNetwork.update_pilots`. This is the code under test. It is a second implementation of
  `engine.rs` step 5 written from SPEC section 3, and section 4 states plainly what that is worth.
- The bidirectional model classes and the two upstream-bug guards (`constraint_current(linear=True)`
  and the `evse_voltage` workaround).

Additions:

| Add | Sketch |
|---|---|
| `trace.py` | Read `trace.csv` into `{slot: [TraceRow]}`; hard-error if `requested_kw` or `emission_index` is absent, naming `TRACE_CONTRACT.txt`; validate internal consistency (a session's `charger_id` is constant over its life; no two concurrent sessions share a `charger_id`; the set of sessions with no rows equals the `never_connected` set in `sessions.csv`). These are input validation, not new coverage: openv2b pins exclusivity itself in `tests/review_regressions.rs`. |
| `replay.py::TraceReplayAlgorithm` | `schedule()` returns `{station_id: [requested_kw]}` for the current slot and calls `interface.set_clamp_order(...)` with stations sorted by `emission_index`, `-1` last. Ordering among the `-1` group is numerically irrelevant (they request 0 and consume no headroom) but must still be deterministic, so sort them by station id. `max_recompute = 1`. Validate per row that `requested_kw != 0 implies emission_index >= 0`, and report a violation as a **harness/contract** failure, not a parity delta. |
| `replay.py::SocAnchor` | Optional per-slot injector using the existing `BidirectionalBattery.set_charge`; records every correction so section 3.2's cross-check can run. |
| `network.py` instrumentation | `update_pilots` must record, per (slot, station), the `requested_in` it received, the `applied_out` it realized, the entry SoC, and the slot aggregates, so the **plugin-side** clamp ledger of 3.8 can be computed from data the plugin generated rather than from openv2b's trace. Recording only; no behavior change. |
| `scenario.py` bridge rework | Assignment from the trace; battery `capacity = max_soc_kwh or battery_kwh` with `battery_kwh` carried separately as `true_capacity_kwh` for reporting; `max_soc_kwh` added to the reader **and to `write_openv2b_scenario`'s `vehicles.csv` header**; dropped sessions excluded from the event stream but retained as phantom links in the persistence chain (openv2b sets `chain_soc[vehicle_id] = arrival_soc` for a dropped session, `src/engine.rs:143`); a named refusal for T11/T12 (arrival SoC above the operating ceiling) rather than an opaque `ValueError` out of `Battery.__init__`. |
| `_xval.py` comparison | Add per-slot per-session applied power and end-of-slot SoC (from `trace.csv`, the finest available granularity; the existing comparison stops at aggregates), plus the clamp-class coverage ledger of section 3.8, plus first-divergence labelling. Its current session-key comparison raises on any key-set mismatch; restrict it to the **connected** key set and assert separately that `openv2b_keys - acnsim_keys` equals exactly the `never_connected` set from `sessions.csv` (X9 depends on this, and a bare `symmetric_difference` would fail the experiment for the one reason it is allowed to differ). |
| `tests/` rework | `tests/test_scenario.py` currently tests `_replay_charger_assignment`. Those tests are **rewritten** to cover trace-derived assignment and the new refusals, not deleted. Deleting tests to make a change land is how coverage disappears quietly. |

### 3.7 The X matrix after the translation layer

Tolerance stays `1e-6`; T8 says the interface itself contributes zero error, so any residual is
real arithmetic.

**Surviving with the same meaning:**

| ID | Purpose | Policies replayed | Green means |
|---|---|---|---|
| **X1** | unit and time-convention mapping; the I/O-contract canary. Run first. | `uncontrolled` x {eta 1.0, eta_c 0.92} | all compared quantities <= 1e-6; delivered energy > 0. Red here means columns, units or CSV schema moved, not physics. It is now also the **trace-schema canary**: a missing new column fails here first, loudly. |
| **X1b** (new in the earlier draft, retained) | operating-ceiling parity | `uncontrolled` | <= 1e-6 **and** departure SoC equals `max_soc_kwh` (< `battery_kwh`) **and** at least one slot has `requested_kw > power_kw` with the ceiling as the binding clamp. Now genuinely non-trivial: the ACN-Sim battery's capacity is the ceiling, so a request that overshoots must clamp identically on both sides. |
| **X3** | discharge dynamics, DR window boundary, banking, no-export | `policy-1`, `edf`, `llf` | <= 1e-6 on signed power, net load, exported kWh, per-slot SoC; exported energy > 0; net load >= 0 in both for every slot; the `(start, end]` boundary slots agree exactly |

**Surviving with a changed meaning (this must be stated in the plugin README, not just here):**

| ID | Was | Is now |
|---|---|---|
| **X2** | contention and arbitration, but the `edf` leg was going to be deleted because no one wanted to maintain an EDF mirror | contention and arbitration, replaying `edf` and `llf` **directly**. This is strictly better: their emission order is non-canonical, which is what makes the clamp-order mutation detectable, and no mirror is required to get it. The mutation "reverse the engine's rationing loop" is still killed: `emission_index` is unchanged by it, so ACN-Sim reproduces the *original* allocation and diverges. The fixture **must** set `heuristic_threshold_kw` in the manifest (via `write_openv2b_scenario`'s `**manifest_extra`) to a value above the building load, or the fallback threshold is exceeded at the top of the budget walk, only the force-charge path fires, and the emission order degenerates toward canonical, which is exactly the fixture the experiment must not have (section 0.3 predicts this for the current fixture). |
| **X4** | heterogeneous ports **and** the assignment-replay test | heterogeneous ports **only**: per-port directional limits and the `min(vehicle, port)` rule. It no longer validates assignment at all, because assignment is now an input. |
| **X4b** | a new fixture whose whole purpose was to falsify a wrong assignment rule | **deleted.** It cannot exist: there is no second assignment implementation to falsify. Assignment coverage moves entirely to openv2b's own tests (`tests/mutation_kills.rs:196` pins the bidirectional-preference pick, `tests/review_regressions.rs:265` pins exclusivity) and to `referee.py`'s re-simulation, which re-derives assignment independently for the seven heuristics (F16). |

**New, and impossible under mirrors:**

| ID | Fixture | Replays | Why it could never have existed before |
|---|---|---|---|
| **X9** | openv2b's **shipped** `examples/one_day`, plus a 7-day slice of `examples/one_month` and of `examples/one_month_lossy` | `uncontrolled`, `policy-1`, `llf` | The bridge previously refused all three, because they contain `never_connected` sessions that acnportal cannot represent (KNOWN_ISSUES item 1, and the largest coverage hole in the whole effort). With the trace reporting assignment, a never-connected session is *identified exactly* instead of re-derived: it has no trace rows, contributes zero to every compared aggregate, and passes its arrival SoC through the persistence chain unchanged. It is excluded from the ACN-Sim event stream and the exclusion set is asserted equal to `sessions.csv`'s `never_connected` set. The refusal collapses from "cannot cross-validate real scenarios" to "one declared, verified exclusion". |
| **X10** | a DR-window fixture with a binding firm level | `mpc` (requires `--features solver-highs`) | KNOWN_ISSUES item 4: there is no MPC mirror and there was never going to be one. Under replay the policy is irrelevant, so an MPC trace is just another setpoint sequence, and it exercises fractional, non-extremal setpoints that heuristics never produce, which is a different corner of the clamp cascade. |
| **X11** | a surplus fixture with staggered departures, deep discharge to the SoC floor, and a saturating no-export guard | `oracle` (requires `--features solver-highs`), plus `policy-1` on the same fixture | Same reason. This is the regime where the sibling OPTIMUS project found a real receding-horizon bug, so it is the regime worth having a foreign engine look at. |

**Still out of scope, with the reasons updated:**

- **X5 (itemized-bill parity).** Unchanged verdict, sharper reason. acnportal has no netting, no
  demand charge over a netted series, and no DR settlement, so an ACN-Sim "bill" would be 100%
  plugin code written from SPEC section 5, with none of ACN-Sim's independence: it would be a second
  `referee.py`, and `referee.py` already recomputes every bill term on every PR (F16). Keep it as
  the largest *listed* gap in `KNOWN_ISSUES.md`, with this reason replacing the previous one.
- **X6 (the ACNSIM_V2B_PLAN item: MPC information-loss canary).** Distinct from X10 above. It is a
  planned-versus-realized drift property, i.e. an openv2b-internal invariant, so it belongs in a
  Rust test, not in a cross-simulator comparison. Note the ID collision in the erratum (Step 18).
- **`scenario-mpc` replay.** Needs `--futures` fixtures. Deferred, named.

### 3.8 Non-vacuity: the clamp-class coverage ledger

The single largest risk in a replay design is a green run that proves nothing because no clamp ever
bound. This is made mechanical rather than argued.

Each clamp class is detectable from `trace.csv` plus the scenario alone, with a single tolerance
`tol = 1e-9` throughout. Let `L_c = min(vehicle max_charge_kw, port max_kw)` and `L_d =
min(vehicle max_discharge_kw, port max_kw) if port bidirectional else 0`, and let `soc_in` be the
previous row's `soc_kwh` (or the session's `soc_arrival_kwh` on its first slot).

| Class | Detector (fires for a (slot, session) row unless stated) |
|---|---|
| `PORT` | `requested_kw > port max_kw + tol` (charge) or `-requested_kw > port max_kw + tol` (discharge), **and** the port limit is the tighter of the two limits |
| `VEHICLE` | same, with the vehicle limit tighter than the port limit |
| `ROOM` | `requested_kw > 0` and `power_kw + tol < min(requested_kw, L_c)` and `power_kw` equals `(ceiling - soc_in)/eta_c` converted to kW, within `tol` |
| `CEILING` | `ROOM` fires **and** the vehicle declares `max_soc_kwh < battery_kwh` |
| `FLOOR` | `requested_kw < 0` and `-power_kw + tol < min(-requested_kw, L_d)` and `-power_kw` equals `(soc_in - floor) * eta_d` converted to kW, within `tol` |
| `DIR` | `requested_kw < 0` and `L_d == 0` (the port or the vehicle does not support discharge), so the request is zeroed on direction grounds |
| `SITE_CAP` | slot-level: aggregate charge equals `max(cap - building_kw, 0)` within `tol`, **and** some session in the slot has `requested_kw > power_kw + tol` |
| `NO_EXPORT` | slot-level: aggregate discharge equals `building_kw + aggregate charge` within `tol`, **and** some session has `-requested_kw > -power_kw + tol` |
| `NONE_BINDING` | slot-level: `power_kw == requested_kw` within `tol` for every session in the slot |

`PORT`/`VEHICLE` are made mutually exclusive by the tie-break clause so that a fixture where the two
limits happen to be equal cannot claim both; if they are equal the harness records neither and the
declaration fails, which is the right outcome (such a fixture distinguishes nothing).

**The ledger must be two-sided, and this is not a refinement, it is the point.** A first draft of
this section computed the ledger from openv2b's trace alone and claimed it would catch a plugin
that replayed `power_kw` instead of `requested_kw`. It would not: the ledger would be unchanged
(it reads the trace, not the plugin), the plugin would feed already-feasible values, ACN-Sim would
clamp nothing, every delta would be **zero**, and the run would be green. That is precisely the
vacuous green the section exists to prevent, so the design has to be:

1. Run the identical detector function twice: once over openv2b's `(requested_kw, power_kw,
   soc_in, limits, slot aggregates)` from `trace.csv` and `slots.csv`, and once over the
   **plugin's own** `(requested_in, applied_out, soc_in, limits, slot aggregates)` recorded by
   `V2BChargingNetwork.update_pilots` during the replay.
2. Assert the two ledgers are **equal per (slot, session)**, not merely non-empty. A class label
   is a statement about *which constraint bound*, so two engines that agree on the number while
   disagreeing on the reason (openv2b clamped by battery room, the plugin by port limit, and the
   values happened to coincide) is a finding, not a pass.
3. Assert that every class each experiment **declares** fired on **both** sides.

Under that design a plugin fed applied power produces an empty plugin-side ledger against a
non-empty openv2b-side ledger, and step 2 fails immediately with the exact slot. T13's pass-membership
asymmetry does not interfere: `DIR` exists precisely so the "negative request on a unidirectional
port" case has one label on both sides rather than being classified differently by each.

The openv2b side alone still certifies *fixture adequacy* (the scenario really does drive the
engine into each declared regime); the two-sided equality is what certifies that the plugin
reached the same answer for the same reason.

Minimum declarations: X1 `{NONE_BINDING, ROOM}`; X1b `{CEILING}`; X2 `{SITE_CAP, PORT}`;
X3 `{NO_EXPORT, FLOOR}`; X4 `{PORT, VEHICLE, DIR}`; X9 `{ROOM, PORT}` at minimum, plus whatever the
shipped examples actually produce, measured not guessed; X10 `{SITE_CAP}`; X11 `{FLOOR, NO_EXPORT}`.

On top of the ledger, the red-team pass (Step 15) requires a **demonstrated** mutation per
experiment. A check with no demonstrated red is a check that does not exist.

---

## 4. The independence claim, restated

Co-locating the two simulators in one repository does not by itself weaken the claim, but the
translation layer **changes its shape**, and the README's current sentence is no longer true as
written. Getting this right matters more than any other paragraph in this plan, because
overclaiming here is the failure mode that actually damages a project's credibility.

### 4.1 What the claim was, and why it must change

The root README says today: *"because the two simulators share no code, agreement between them is
evidence of correctness rather than of a shared bug"*, followed by *"per-slot power and per-session
energies agree to max |delta| = 0.0"*.

Two problems. First, it was already an overstatement: `docs/SPEC.md` is the common ancestor of both
implementations, so agreement never ruled out a shared misreading of the spec. Second, and new: it
described an **end-to-end** comparison, where the plugin computed openv2b's outputs from the
scenario alone. Under replay the plugin cannot do that, because it consumes openv2b's own
decisions. The claim becomes narrower and, for the first time, entirely true.

### 4.2 The claim, in three tiers

The plugin's stack is not uniformly independent, and pretending otherwise is the easiest mistake
available here. Stated by layer:

**Tier 1: genuinely third-party.** Written by the acnportal authors, unmodified, and exercised as
released:

- the `Simulator` loop, the event queue and its precedence rules (`UnplugEvent` before
  `PluginEvent` at equal timestamps, which independently confirms openv2b's
  departures-before-arrivals convention and its half-open session interval);
- station registration, the voltage/current infrastructure, and pilot range validation
  (`EVSE._valid_rate`);
- the schedule-to-pilot pipeline and acnportal's own `Simulator.charging_rates` recording, which
  the harness reads as a **second, independent accounting path** and diffs separately from the
  network's own slot records.

**Tier 2: a second implementation by the same author from the same specification.** No more
independent than `tools/referee.py`, and it must be described that way:

- the SoC recursion and the efficiency split (`BidirectionalBattery.charge`, written from SPEC
  section 3);
- the site-cap rationing and the no-export guard (`V2BChargingNetwork.update_pilots`, written from
  SPEC section 3);
- the drawn/exported energy split (`V2BEV.charge`).

**Tier 3: not cross-validated by this leg at all.** Consumed as inputs, by design:

- the policies (deliberately: that is the entire point of the redesign);
- the setpoint filter and last-write-wins dedup;
- the emission order;
- charger assignment;
- billing.

### 4.3 So what is agreement actually worth?

Precisely this: **openv2b's outputs are re-integrated by a differently-structured program, with
different data structures, a different unit basis (Amps times volts rather than kW, T14), a
different accumulation order, and a different iteration driver, running inside a third-party event
loop.** Agreement therefore rules out the transcription/unit/ordering/off-by-one/accumulation
class of bug in the compared layer. It does **not** rule out a shared misreading of `docs/SPEC.md`,
because tier 2 descends from the same document, and since commit `a743277` that document itself
encodes the reference simulator's algorithms.

Say it in one sentence in the README: **engine independence, not end-to-end independence.**

A reviewer will reasonably ask why this is worth having at all given `tools/referee.py`. The honest
answer, including its weakness:

1. `referee.py` is kW-native like openv2b, so it cannot catch a unit-conversion error. The
   Amps-times-volts path can (T14).
2. `referee.py` implements its own time loop from the same prose; acnportal's loop, event
   precedence and session-interval semantics are third-party and were not written for openv2b.
3. `Simulator.charging_rates` is a completely separate accounting path from the network's slot
   records, and the harness diffs both.
4. Independent of epistemics: a recognized third-party simulator is what a reviewer of a paper will
   ask for, and "we ran it through ACN-Sim" is answerable only by having done it.
5. **Against**: for the tier-2 layer the epistemic delta over `referee.py` is modest, and the two
   share a spec ancestor. The right description is "a second, structurally different check with a
   partly third-party substrate", not "an independent oracle".

Write points 1-5 into `xval/README.md`. A reviewer who finds that hole themselves discounts
everything else; a reviewer who finds it already written down does not.

Cost of deferring `decisions.csv` (section 3.5): the filter/dedup/emission-order rules stay in
tier 3 rather than moving to tier 2. Named here so the tier table stays honest if the deferral
becomes permanent.

### 4.4 What must be true after the move, and stay true

1. **No build-time or run-time coupling.** `src/**` contains no reference to `xval`, `acnportal`, or
   Python beyond the existing stdlib tools. `cargo build`, `cargo test`, `cargo clippy` never touch
   `xval/`. The published crate excludes it. The two new `TraceRecord` fields are documented as
   general external-verification output and name no consumer.
2. **The plugin drives openv2b only as a black box.** It invokes the release binary as a subprocess
   and parses `slots.csv` / `sessions.csv` / `trace.csv` / `summary.json`. No Python file under
   `xval/` may read, import, parse, or transcribe anything under `openv2b/src/`.
3. **No policy code in the plugin.** No file under `xval/` may contain a scheduling decision. The
   mechanical proxy: no `.py` under `xval/` may name an openv2b policy except in a docstring, a
   comment, or `REPLAYED_POLICIES.txt`.
4. **Tier-2 code is written from prose, not from Rust**, and each piece records its prose source and
   date in `DERIVATION.md`.
5. **No expectation flows the other way.** No file under `tests/`, `examples/`, `tools/`, or `src/`
   may be generated by, copied from, or hand-transcribed out of the plugin.
6. **Outputs are never fed back as inputs** (section 3.2's governing rule), with the anchored mode
   as the single declared exception, which never carries the claim.
7. **The claim is scoped, tiered, and versioned.** The README states which experiments agree, at
   which openv2b commit, with which binary sha256, and under which tier. A bare "max |delta| = 0.0"
   with no scope is what went stale the first time.

### 4.5 What would falsify it

Any one of these, and the claim comes out of the README the same day:

- a scheduling decision appears in the plugin, or `heuristics.rs` is transcribed into Python;
- the plugin's expected values are vendored into Rust fixtures;
- the plugin calls openv2b as a library (PyO3, FFI) rather than a subprocess;
- openv2b grows a `build.rs`, cargo alias, or test that shells into `xval/`;
- the free-running mode is quietly dropped and only anchored numbers are reported;
- a parity gap is closed by loosening `_xval.TOLERANCE` instead of by naming the divergence.

### 4.6 Mechanical checks

`tools/check_xval_sync.py` (new, stdlib only, runs in the existing cheap CI job on every PR):

| # | Assertion | Kills |
|---|---|---|
| C1 | `xval/acnportal-v2b/LICENSE` exists and contains `BSD 3-Clause` | deletion or relicensing |
| C2 | Root `Cargo.toml` `[package]` has an `exclude` entry matching `xval` | someone drops the exclude while editing the manifest |
| C3 | Root `README.md` contains the BSD-3 exception sentence **and** the phrase `engine independence` | the license story or the scoped claim rotting out of the front page |
| C4 | No file under `src/`, `tests/`, `examples/` mentions `acnportal` or `xval` | reverse coupling; vendored expectations |
| C5 | No file under `xval/**/*.py` references a path under openv2b's `src/` or a `.rs` file | Rust transcription |
| **C6a** | The `pub <name>: <type>` field set parsed out of `pub struct TraceRecord` in `src/engine.rs` is a **superset** of `TRACE_CONTRACT.txt`, and the declared types match | **the replacement tripwire.** A renamed, retyped or removed trace column silently breaks replay; this catches it on the PR that does it, before the heavy job runs |
| **C6b** | Every name in `REPLAYED_POLICIES.txt` appears either in `POLICY_NAMES` (`src/policy/mod.rs`) or as a quoted match arm in `src/main.rs` | a replayed policy name is renamed or deleted (F1, F2) |
| C7 | Every `.py` under `xval/` starts with the SPDX line | a new file lands without a license marker |
| C8 | No `.py` under `xval/` matches an openv2b policy name outside a comment, docstring, or `REPLAYED_POLICIES.txt` | a mirror creeping back in (4.4 item 3) |

C6a is strictly stronger than the policy-name tripwire the earlier draft proposed, because it
guards a *data contract* rather than a semantic claim, and a data contract is mechanically
parseable. C6b's residual: a name that exists but is feature-gated off (`mpc` in a default build)
passes C6b and fails at run time. That is handled by section 5.3's job split, not by the checker,
and it is why the heavy job must actually run.

Limits, stated rather than glossed:

- C6a cannot see a field whose *semantics* changed under an unchanged name and type (e.g.
  `requested_kw` becoming post-clamp). That is covered by running the suite: X1's clamp-class ledger
  would report `NONE_BINDING` everywhere and fail on its declared classes.
- C5 and C8 are textual heuristics and are evadable by anyone determined. The real control is the
  written rule plus review; the checker catches accidents. Say so rather than overselling it.
- These checks are Python scripts in the cheap CI job rather than Rust `#[test]`s on purpose: a Rust
  test would have to read files that `exclude` removes from the packaged crate, which breaks
  `cargo package --verify` and anyone vendoring the crate.

---

## 5. CI design

### 5.1 Constraints

- The heavy job needs CPython 3.10 plus acnportal 0.3.3, numpy 1.26, pandas 1.5, setuptools 80,
  which transitively pulls matplotlib, scipy and scikit-learn: roughly 2-4 minutes of installation.
- It needs `cargo build --release`, ~2 minutes cold, well under a minute with `Swatinem/rust-cache`
  (already in use).
- X10/X11 additionally need `--features solver-highs`, which builds HiGHS through cmake. Cold cost
  is minutes and is **unmeasured**; see 5.3.
- X9's month slice runs 183 sessions, so 183 EVSEs, over ~672 slots. acnportal is not fast. Also
  unmeasured; see R12.
- openv2b's existing CI is a fast Rust-plus-stdlib-Python job that contributors expect to stay fast.

So: **two workflows, and inside the heavy one, two jobs.**

### 5.2 Triggers

```yaml
# .github/workflows/xval.yml
name: Cross-validation (ACN-Sim)

on:
  workflow_dispatch:
  schedule:
    - cron: "17 6 * * 1"        # Mondays 06:17 UTC: catches dependency rot, not code churn
  push:
    tags: ["v*"]                # release gate: a tag must not ship a stale parity claim
  pull_request:
    paths:
      - "src/engine.rs"         # the code under test
      - "src/state.rs"          # Observation / Setpoint: the request contract
      - "src/scenario.rs"       # input semantics the bridge mirrors
      - "src/output.rs"         # the trace/slots/sessions schema
      - "src/policy/**"         # changes the traces being replayed
      - "src/milp/**"           # changes the mpc/oracle traces
      - "Cargo.lock"            # a serializer bump could change f64 formatting (T8)
      - "xval/**"
      - ".github/workflows/xval.yml"

concurrency:
  group: xval-${{ github.ref }}
  cancel-in-progress: true
```

Trigger rationale, one line each:

- `pull_request` + `paths`: the only PRs that can break parity are the ones touching the engine, the
  I/O contract, the input semantics, the policies, the solver, or the plugin. Everything else stays
  on the fast path. `Cargo.lock` is in the list because T8's bit-exactness is a property of the
  serializer.
- `schedule`: parity can break with no code change at all (a yanked wheel, a new setuptools). Weekly
  is enough; the failure mode is slow, not urgent.
- `push: tags`: a release is the moment the README's claim gets read by strangers.
- `workflow_dispatch`: needed for the red-team verification in section 6 and for one-off debugging.

**Do not mark this workflow a required status check.** A path-filtered workflow that does not run
produces no check at all, and a required check that never arrives blocks every unrelated PR forever.
If a required gate is wanted later, the standard pattern is a second always-running job that
`needs:` the filtered one and passes when it is skipped; that complexity is not justified yet.

`schedule` triggers are disabled by GitHub after 60 days of repository inactivity (with an email to
the owner). The tag trigger does not rot, which is why both exist.

### 5.3 Two jobs: solver-free and solver

`xval` (default features) runs X1, X1b, X2, X3, X4, X9. `xval-solver`
(`cargo build --release --features solver-highs`) runs X10 and X11.

The open question is whether `xval-solver` runs on pull requests. It is a **measured** decision, not
a guessed one:

- Measure the cold and warm `--features solver-highs` build in Step 6 (`workflow_dispatch` on a
  scratch branch, twice).
- **If cold <= 8 minutes and the warm cache hits reliably**: run `xval-solver` on the same triggers
  as `xval`. The path filter already restricts it to engine/policy/solver/plugin PRs.
- **Otherwise**: `if: github.event_name != 'pull_request'` (schedule, tag and dispatch only), and
  record the resulting gap explicitly: *an MPC-affecting PR can merge without an MPC replay run
  until the next Monday, and releases are still gated by the tag trigger.* That gap is acceptable,
  but it must be written in `xval/README.md`, not left implicit.

Do not use a third-party changed-files action to split the difference: a supply-chain dependency to
save four minutes is a bad trade in a repository whose selling point is provenance.

### 5.4 Job body (the solver-free job)

The solver job runs the **same steps** with three differences: `cargo build --release --features
solver-highs`; `-m solver` instead of `-m "not solver"`; and `XVAL_MIN_TESTS: "2"`. X10 and X11 are
marked `@pytest.mark.solver`, registered in `pyproject.toml`'s `markers` so `--strict-markers`
catches a typo, and the two jobs' selections must partition the suite: a test that is neither
selected by `-m solver` nor by `-m "not solver"` cannot exist, but a test *file* that no job runs
can, so Step 15's floors are the guard. Both jobs carry the scheduled-failure reporter, so a
double failure files two issues; that is acceptable and better than a shared reporter that could
attribute a solver failure to the parity suite.

```yaml
jobs:
  xval:
    runs-on: ubuntu-latest
    timeout-minutes: 30          # raised from 25: X9's month slice is unmeasured (R12)
    permissions:
      contents: read
      issues: write              # only used by the scheduled-failure reporter below
    steps:
      - uses: actions/checkout@v4

      # --- the binary under test is built from THIS commit, never downloaded ---
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build openv2b
        run: cargo build --release
      - name: Record binary provenance
        run: |
          echo "openv2b commit: $(git rev-parse HEAD)"          | tee -a "$GITHUB_STEP_SUMMARY"
          echo "binary sha256:  $(sha256sum target/release/openv2b | cut -d' ' -f1)" \
                                                                 | tee -a "$GITHUB_STEP_SUMMARY"

      # --- the trace contract, checked against the binary that will produce the traces ---
      - name: Trace schema smoke test
        run: |
          ./target/release/openv2b --scenario examples/one_day --policy uncontrolled --out /tmp/tc
          head -1 /tmp/tc/trace.csv | tee -a "$GITHUB_STEP_SUMMARY"
          for col in requested_kw emission_index; do
            head -1 /tmp/tc/trace.csv | tr ',' '\n' | grep -qx "$col" \
              || { echo "trace.csv lacks $col: see xval/acnportal-v2b/TRACE_CONTRACT.txt"; exit 1; }
          done

      - uses: actions/setup-python@v5
        with:
          python-version: "3.10"
          cache: pip
      - name: Install the plugin (hash-pinned)
        working-directory: xval/acnportal-v2b
        run: |
          python -m pip install --upgrade "pip<25" "setuptools<81" wheel
          pip install --require-hashes -r requirements.lock.txt
          pip install --no-deps -e .
          python -c "import acnportal.acnsim, acnportal_v2b; print(acnportal.__version__)"

      - name: Cross-validation suite
        working-directory: xval/acnportal-v2b
        env:
          # Set explicitly: the plugin's discovery order would otherwise fall back to
          # ~/openv2b/target/release/openv2b and could validate a stale binary on a
          # developer machine. In CI it must be this build or nothing.
          OPENV2B_BIN: ${{ github.workspace }}/target/release/openv2b
        run: |
          set -o pipefail
          python -m pytest -q -rs -m "not solver" --strict-markers \
            --junitxml=xval-report.xml 2>&1 | tee -a "$GITHUB_STEP_SUMMARY"

      # `pytest -rs` only REPORTS skips; it does not fail on them, and it exits 0
      # when zero tests are collected. Both are false greens for a job whose whole
      # purpose is to have run something. Assert on the machine-readable report.
      - name: Assert the suite actually ran
        if: always()
        working-directory: xval/acnportal-v2b
        run: |
          python - <<'PY'
          import sys, xml.etree.ElementTree as ET
          import os
          root = ET.parse("xval-report.xml").getroot()
          suites = [root] if root.tag == "testsuite" else list(root)
          total   = sum(int(s.get("tests", 0))   for s in suites)
          skipped = sum(int(s.get("skipped", 0)) for s in suites)
          floor   = int(os.environ["XVAL_MIN_TESTS"])
          print(f"collected={total} skipped={skipped} floor={floor}")
          if total < floor:
              sys.exit(f"only {total} tests ran, expected at least {floor}")
          if skipped:
              sys.exit(f"{skipped} skipped test(s): a skipped cross-validation is a false green")
          PY
        env:
          # Per JOB, not per suite: the solver-free job runs X1, X1b, X2, X3, X4, X9
          # plus the unit tests; the solver job runs only X10 and X11. A single shared
          # floor would fail one of them. Step 15 owns this number.
          XVAL_MIN_TESTS: "6"

      - name: Upload run artifacts
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: xval-runs-${{ github.sha }}
          path: xval/acnportal-v2b/xval_runs/
          if-no-files-found: warn

      - name: File an issue on scheduled failure
        if: failure() && github.event_name == 'schedule'
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          gh issue create \
            --title "xval: scheduled cross-validation failed ($(date -u +%F))" \
            --body "Run: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}

          Triage order:
          (1) install step failed  -> dependency rot, not parity.
          (2) trace schema smoke test failed -> a trace column moved; see
              xval/acnportal-v2b/TRACE_CONTRACT.txt and tools/check_xval_sync.py C6a.
          (3) a replay raised CalledProcessError -> a replayed policy name no longer
              exists (C6b), or the solver feature is missing, or the MILP itself failed
              ('oracle solve failed' / 'requires --features solver-highs' on stderr).
              A solver failure is NOT a parity failure and must not be reported as one.
          (4) a clamp-class coverage assertion failed -> the fixture stopped exercising
              its guard; the run is vacuous, not merely red.
          (5) otherwise read the first divergence slot in the diff table." \
            --label xval
```

The trace schema smoke test is deliberately in the job body as well as in `check_xval_sync.py`: C6a
reads the source, this reads the *built binary's actual output*, and only the second one catches a
mismatch between what `engine.rs` declares and what `output.rs` writes.

### 5.5 How failures surface

| trigger | surfacing |
|---|---|
| `pull_request` (path-filtered) | ordinary red check on the PR + the diff table in the job summary |
| `workflow_dispatch` | red run + job summary |
| `schedule` | red run + **an auto-filed issue**, because nobody watches a cron run |
| `push: tags` | red run on the tag; the release must not be published until it is green |

The auto-issue is gated on `github.event_name == 'schedule'` on purpose: a `pull_request` run from a
fork has a read-only token and `gh issue create` would fail, turning a parity failure into a
confusing permissions error.

Diff tables go to `$GITHUB_STEP_SUMMARY` (readable without expanding logs) and `xval_runs/` is
uploaded so a failure can be reproduced locally from the exact scenario directories and traces.

### 5.6 Cheap per-PR additions to `ci.yml`

Append one step to the existing `test` job (no new dependencies; `python3` is already required):

```yaml
      - name: xval sync + license invariants
        run: python3 tools/check_xval_sync.py
```

This is the piece that runs on **every** PR and is the actual anti-drift mechanism (section 4.6).

### 5.7 Pinning against dependency rot

`requirements.txt` already carries exact `==` pins, but `==` does not protect against a re-uploaded
or yanked artifact. Generate a hash-pinned lock once and commit it:

```bash
cd xval/acnportal-v2b
uv pip compile --generate-hashes --python-version 3.10 requirements.txt -o requirements.lock.txt
# or: pip-compile --generate-hashes --output-file requirements.lock.txt requirements.txt
```

`requirements.txt` stays the human-readable rationale document (it explains *why* each bound
exists); `requirements.lock.txt` is what CI installs. Both are committed. The lock must be
regenerated whenever `requirements.txt` changes, and the release gate re-runs the whole install from
scratch, which is where a drifted lock shows up.

Documented degradation path, so the project stays honest when this eventually breaks: Python 3.10
reaches end of life in October 2026 and acnportal 0.3.3 has had no upstream release since
2023-11-21. If the environment becomes uninstallable the claim does **not** silently rot: it becomes
a *historical* claim, `RESULTS.md` records the last green openv2b commit and run URL, the README
says so in the past tense, and `xval.yml`'s schedule is disabled deliberately rather than left red.

---

## 6. Execution checklist, with a verification that could fail

Each step lists the command that proves it worked. Where a check could pass for the wrong reason, a
**negative control** is specified: deliberately break the thing, confirm the check goes red, restore.

### PR0: the trace-schema extension (openv2b only, no plugin involved)

| # | Step | Verification (and negative control) |
|---|---|---|
| **0** | **Measure before touching anything.** In the existing plugin repo, with the existing binary, run all four experiments and capture the output verbatim to `/tmp/xval_baseline.txt`. Also record `gh api repos/rishavsen1/openv2b --jq '.license.spdx_id'` for the Step 20 comparison. | The captured output matches section 0.3: X1 green, X2/`uncontrolled` green, X2/`edf` red, X3 and X4 crash on `edf-v2b`. If X1 is red, stop: the I/O contract moved and that is a different problem (abort A1). |
| 1 | Update `docs/SPEC.md`: section 6's `trace.csv` sentence gains the two columns; section 3 gains one sentence stating that the deduplicated request and its emission position are observable in `trace.csv`. Spec first, per the project's own rule. | `rg -n 'requested_kw\|emission_index' docs/SPEC.md` returns both. |
| 1b | **Document `max_soc_kwh` in prose.** It appears nowhere in `docs/SPEC.md` (section 2's entity list stops at capacity/target/floor/limits, section 6's validation list never mentions it) and nowhere in `docs/INPUT_FORMAT.md`; its only description is the doc comment on `src/scenario.rs`. Invariant 4.4.4 requires tier-2 plugin code to be written from prose, so **as things stand the ceiling cannot be implemented in the bridge without reading Rust**, which would falsify the independence claim at the exact point X1b is meant to test. Add to SPEC section 2 (the operating ceiling, distinct from `battery_kwh`, and that the >90% taper anchors to the true capacity, not the ceiling), section 6 (the validation rule: arrival SoC is checked against `battery_kwh`, **not** against the ceiling, so an arrival above the ceiling is legal and simply cannot charge, T11), and to `docs/INPUT_FORMAT.md`'s `vehicles.csv` column table. Confirm `heuristic_threshold_kw` is likewise documented as a manifest key. | `rg -n 'max_soc_kwh' docs/SPEC.md docs/INPUT_FORMAT.md` returns hits in both. This step is a **precondition for X1b**, not a nicety: without it, Step 14's `DERIVATION.md` entry for the ceiling would have no prose source to cite. |
| 2 | Add `requested_kw` and `emission_index` to `TraceRecord` and fill them from the final `requested` vector. | `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`. Then: `./target/release/openv2b --scenario examples/one_day --policy policy-1 --out /tmp/t && head -1 /tmp/t/trace.csv` shows both columns. **Negative control on T9:** `rg -n 'TraceRecord *{' --glob '*.rs' src tests` finds exactly one construction site (in `engine.rs`). |
| 3 | Add one Rust test pinning T5 and T6: a policy that emits (a) an out-of-range index, (b) a finite setpoint for session 0, (c) a NaN for session 0, (d) a setpoint for session 1, (e) a second setpoint for session 0. Assert session 0's `requested_kw` is the value from (b) (the NaN was discarded, the earlier finite value survived) unless (e) supersedes it, that (e)'s value wins with `emission_index` **after** session 1's, and that a session the policy never named has `requested_kw == 0.0` and `emission_index == -1`. | `cargo test` green. **Negative control:** change the dedup to keep the first setpoint; the test must fail. |
| 4 | (Recommended, 3 lines) Add `referee.py`'s policy-agnostic `M-clamp` check. | `python3 tools/referee.py examples/one_day /tmp/t` passes; **negative control:** hand-edit one `power_kw` in `/tmp/t/trace.csv` above its `requested_kw`, confirm the check fires, restore. |
| 5 | Note in the PR0 message that `reports/OVERNIGHT_REPORT.*` sha values no longer reproduce, and why. | `rg -n 'sha' reports/OVERNIGHT_REPORT.md \| head` shows the affected lines; PR0's body names them. |
| 6 | Measure the `--features solver-highs` build, cold and warm, on a scratch branch via `workflow_dispatch`, to settle section 5.3. | Two run durations recorded in the PR1 description. This is an input to a decision, so it must be a number, not an impression. |

### PR1: the fold

| # | Step | Verification (and negative control) |
|---|---|---|
| 7 | Bundle + hash-list the plugin repo (section 1.4 safety rails). | `git bundle verify ~/acnportal-v2b-preimport.bundle` prints "The bundle is valid"; the hash file has 8 lines. |
| 8 | Rewrite the clone and import onto `xval-fold`. | `git log --oneline -- xval/acnportal-v2b \| wc -l` >= 9 (8 rewritten + merge). `git show --stat HEAD~1 \| head` shows `xval/acnportal-v2b/...` paths. **Negative control:** on a plain `git subtree add` the same count is 1; if you get 1, the rewrite did not happen. |
| 9 | Confirm nothing untracked leaked in. | `git status --porcelain` empty; `find xval -name '__pycache__' -o -name '.venv' -o -name '*.egg-info' -o -name 'xval_runs'` prints nothing. |
| 10 | Add `exclude = ["/xval"]` to `Cargo.toml`. | `cargo package --list --allow-dirty \| grep -c '^xval/'` prints `0`. **Negative control:** comment the `exclude` out, re-run, confirm > 20, restore. Also `cargo package --list --allow-dirty \| grep -c 'tools/referee.py'` must still print `1` (proves the exclude did not over-reach). |
| 11 | Licensing artifacts: SPDX headers, `xval/README.md` (including section 4.3's five points), plugin README header, root README exception sentence, `CONTRIBUTING.md` exception, `docs/PROVENANCE.md` subsection, `.gitattributes`. | `rg -c 'SPDX-License-Identifier: BSD-3-Clause' xval/acnportal-v2b --glob '*.py' \| wc -l` equals the `.py` file count; `rg -q 'xval/acnportal-v2b' README.md CONTRIBUTING.md docs/PROVENANCE.md` succeeds for all three. |
| 12 | Write `TRACE_CONTRACT.txt` and `REPLAYED_POLICIES.txt`. | `TRACE_CONTRACT.txt` lists all eight trace fields with types; `REPLAYED_POLICIES.txt` lists exactly the names used by the X matrix. |
| 13 | Write `tools/check_xval_sync.py` (C1-C8) and wire it into `ci.yml`. | `python3 tools/check_xval_sync.py` exits 0. **Negative controls, one per check, all restored:** rename the plugin `LICENSE` (C1); delete the `exclude` line (C2); remove the "engine independence" phrase from the README (C3); add the word `acnportal` to a file under `src/` (C4); rename `requested_kw` in `engine.rs` (C6a); add a bogus name to `REPLAYED_POLICIES.txt` (C6b); drop an SPDX header (C7); write `power = edf_rate` into a plugin `.py` (C8). **A check with no demonstrated red is a check that does not exist.** |
| 14 | Build the translation layer (section 3.6): deletions first, then `trace.py`, `replay.py`, the bridge rework, the comparison extension. Rewrite `tests/test_scenario.py`'s assignment tests rather than deleting them. Record every tier-2 derivation source in `DERIVATION.md` as it is written, not afterwards. | `pytest xval/acnportal-v2b/tests -q -rs` green, and the junit assertion of 5.4 run locally reports zero skips (eyeballing `-rs` output is not the check; the check is the script). `rg -n 'class (EDF\|LLF\|Uncontrolled)\|_replay_charger_assignment\|laxity_slots\|discharge_budget\|headroom_kw' xval/` returns nothing. |
| 15 | Build the X matrix (section 3.7) with the clamp-class ledger (section 3.8): X1, X1b, X2, X3, X4, X9, X10, X11. Delete X4b and the `edf-v2b`/`llf-v2b` legs. | Full suite green **and** every declared clamp class fired. **Then the red-team pass, which is the real verification:** (a) reverse the engine's rationing loop in `engine.rs` (leaving the `requested` vector, and therefore `emission_index`, untouched) -> X2 red on delta; (b) keep `requested_kw` but make the `emission_index` fill always write `-1`, so the plugin falls back to registration order -> X2 red **and** flagged as a contract violation by the `requested_kw != 0 implies emission_index >= 0` rule; (c) ignore `max_soc_kwh` in the bridge -> X1b red; (d) drop the `(start, end]` `+1` in `DrEvent.contains` -> X3 red; (e) change the plugin's discharge conversion from `budget * eta_d` to `budget / eta_d` -> X3/X11 red; (f) make the plugin ignore `charger_id` and assign round-robin -> X4/X9 red; (g) feed `power_kw` instead of `requested_kw` -> **every delta stays exactly zero**, and the only thing that goes red is the two-sided clamp ledger (section 3.8): the plugin-side ledger is empty while the openv2b-side ledger is not. Mutation (g) is the single most important one in this list, because it is the mutation that makes the entire suite green while proving nothing; if (g) does not go red, stop and fix the ledger before trusting any other result. Any mutation that does not produce a red identifies a vacuous experiment, which must be fixed before proceeding. |
| 16 | Generate `requirements.lock.txt` (hash-pinned). | In a throwaway venv: `pip install --require-hashes -r requirements.lock.txt` succeeds, `pip check` is clean, `python -c "import acnportal.acnsim"` succeeds. |
| 17 | Add `.github/workflows/xval.yml` with both jobs, applying Step 6's measurement to the solver job's trigger. | Push the branch, `gh workflow run xval.yml --ref xval-fold` -> green, and **both** jobs appear. **Negative control:** temporarily set `_xval.TOLERANCE = 0.0`, dispatch again, confirm red *and* that the diff table appears in the job summary and `xval_runs/` uploaded. Restore. **Second negative control (local, no pre-PR0 binary needed):** run one experiment, then `cut -d, -f1-6` an emitted `trace.csv` to strip the two new columns and re-run the plugin against that directory; it must abort with the `TRACE_CONTRACT.txt` message, not a `KeyError` and not a silent fallback. **Third negative control:** delete one experiment file and confirm the "assert the suite actually ran" step goes red on the collected-count floor. |
| 18 | Update the claim-bearing prose: root `README.md` (replace the "max \|delta\| = 0.0" paragraph with the tiered claim of section 4.2/4.3 and the words "engine independence, not end-to-end independence"), `CLAUDE.md` ("Cross-validation status" + "Active work"), `docs/VALIDATION.md` section 2, `docs/ROADMAP.md`, `docs/ACNSIM_V2B_PLAN.md` (erratum: the X-matrix policy names `edf-v2b`/`llf-v2b` never existed, and its X5/X6 ids do not match the implemented suite's X9-X11), plugin `README.md`, plugin `KNOWN_ISSUES.md` (items 1 and 4 move to "resolved by the translation layer", item 2's reason is replaced per section 3.7, items 3 and 5 stand), new `RESULTS.md`. | `rg -n 'edf-v2b\|llf-v2b\|60c76bb' README.md CLAUDE.md docs/ xval/` returns only lines inside explicitly historical context. `rg -n 'no open parity gaps' xval/` returns nothing. `rg -n 'share no code' README.md` returns nothing. |
| 19 | Confirm the Rust-only contributor path is untouched. | In an environment where `python3 -c "import numpy"` **fails**: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build --release && python3 tools/referee.py examples/one_day /tmp/out && python3 tools/check_xval_sync.py` all pass. The import failure proves the heavy toolchain really is absent. |
| 20 | Open PR1, **merge with a merge commit** (section 1.5), then check GitHub's rendered license and language bar. | CI green; the xval workflow actually appears on the PR rather than being skipped (`xval/**` changed, so it must run). After merge: `git log --oneline --graph -15` on `main` shows the merged plugin history, not one squashed blob; `gh api repos/rishavsen1/openv2b --jq '.license.spdx_id'` matches the Step 0 value; the repo page does not show Python as the primary language. |
| 21 | Retire the source repo (not before Step 20 is green, and not before an agreed cooling-off period). | `~/acnportal-v2b` is moved to `~/archive/`, not deleted; the Step 7 bundle is kept off-repo. |

Estimated effort: PR0 is half a day plus the Step 6 measurement. In PR1, Steps 7-13 and 16-21 are
half a day; Steps 14-15 (the translation layer, the eight experiments, and the seven-mutation
red-team pass) dominate at 2 to 3 days. That sits between the earlier draft's two options (1-2 days
for the scoped-down mirrors, 3-5 for the full mirror re-sync), and unlike either it buys X9, X10
and X11 as net new coverage and removes the recurring re-sync cost entirely.

### Rollback and abort criteria

Abort before Step 20 (nothing public has changed; delete `xval-fold`, the plugin repo is untouched)
if:

- **A1** Step 0 contradicts section 0.3 in a way that changes the diagnosis, e.g. X1 is red (the I/O
  contract moved and must be fixed first).
- **A2** Step 10's negative control shows `exclude` cannot keep `xval/` out of the crate. Do not
  proceed with a misdescribed package.
- **A3** Step 15's red-team pass cannot find a mutation that an experiment kills, or a declared
  clamp class never fires and no fixture change makes it fire. Then that experiment is vacuous:
  fix the fixture or drop the experiment and say so. Mutation (g) failing to go red is the most
  serious version, because it means the ledger itself is broken.
- **A4** The free-running and anchored modes disagree about the first divergence slot (section 3.2's
  cross-check). That is a harness bug, and no number from either mode may be published until it is
  explained.
- **A5** Any evidence that plugin code was transcribed from `src/**/*.rs`, or that a scheduling
  decision has appeared under `xval/`. Revert that code entirely.
- **A6** X9 cannot run the month slice within the job timeout even after slicing down to one week
  (R12). Then X9 ships with `one_day` only, and `xval/README.md` says the shipped month examples
  remain uncovered, rather than quietly dropping the claim.
- **A7** The xval job's wall time exceeds ~20 minutes or it is flaky more than once in the first
  month. Drop the `pull_request` trigger and keep schedule + tag only.

Rollback after Step 20 (public):

- Prefer **forward fixes**. Reverting a merge commit (`git revert -m 1 <sha>`) leaves the mainline
  poisoned against a future re-merge (git believes the content is already merged), so a later
  re-import needs a revert of the revert or a fresh import.
- The genuinely reversible pieces are `xval.yml` (delete the file), the `ci.yml` step (delete the
  step), and `Cargo.toml`'s `exclude` (which should never be reverted). If the whole fold must be
  undone, prefer `git rm -r xval && git commit -s` plus restoring the standalone repo from the
  Step 7 bundle, and say so in the commit message rather than pretending it never happened.
- PR0 is independently keepable: the two trace columns are useful without the plugin (they are what
  `referee.py`'s `M-clamp` check reads), so unwinding the fold does not require unwinding PR0.

---

## 7. Risks and mitigations

| # | Risk | Mitigation | Residual |
|---|---|---|---|
| R1 | **Replay is vacuous**: the replayed requests are already feasible, so ACN-Sim clamps nothing and agreement is trivial. This is the single most dangerous failure mode of the whole design, because it produces a *green* suite. | Requested-not-applied replay (3.5); the **two-sided** clamp ledger (3.8), which fails when the plugin's clamps did not bind where openv2b's did, or bound for a different reason; per-experiment class declarations; red-team mutation (g), whose only detector is that ledger. | A class can fire in a slot where nothing else is interesting. Mitigated by requiring a *demonstrated* mutation per experiment, not coverage alone. |
| R2 | **The claim is overstated in prose** even though the mechanism is sound. This is the most likely way this effort damages the project. | Section 4's three tiers, the mandatory phrase "engine independence, not end-to-end independence", C3 checking that phrase is present, and section 4.3's five points (including the one arguing *against*) written into `xval/README.md`. | There is no mechanical check for over-claiming in English. C3 checks a phrase, not a meaning. |
| R3 | **Trace-schema drift**: a column is renamed or retyped and replay breaks, or worse, silently changes meaning. | C6a (source-parsed contract, every PR) plus the built-binary smoke test in the job body; the plugin hard-errors instead of falling back. | C6a cannot see a semantics change under an unchanged name and type; caught only by the clamp ledger going `NONE_BINDING`. |
| R4 | **Coverage loss the fold introduces**: assignment, dedup/filter, and emission order leave the cross-validated set (tier 3). | Named in 4.2 rather than hidden; assignment is pinned in Rust (`mutation_kills.rs:196`, `review_regressions.rs:265`) and re-derived by `referee.py` for the seven heuristics; dedup is pinned by SPEC invariant 9 and `mutation_kills.rs`; emission order by `audit_r2.rs` (T10). The `decisions.csv` widening is fully specified in 3.5 if it is ever wanted. | For `mpc`/`oracle`, `referee.py` re-simulates nothing, so tier-3 coverage there rests on Rust tests alone. |
| R5 | **The bridge refuses scenarios by design** (no `never_connected` in acnportal). | Largely **resolved**: X9 excludes never-connected sessions explicitly and asserts the exclusion set equals `sessions.csv`'s. The shipped examples become cross-validatable. | The exclusion is a declared hole: openv2b's handling of a dropped session's persistence pass-through is asserted from `sessions.csv`, not re-derived. Record in `KNOWN_ISSUES.md`. |
| R6 | **acnportal pins rot.** Upstream unmaintained since 2023-11-21; `setuptools>=81` already breaks it; Python 3.10 EOL October 2026. | Hash-pinned lock; weekly schedule; auto-issue distinguishing install failure from parity failure; the documented historical-claim degradation path (5.7). | Eventually unfixable without a container image or vendored wheels. Both are one step from the documented path. |
| R7 | **Rust-only contributors forced into a Python toolchain.** | `xval/` is not a workspace member, has no `build.rs` hook, no cargo alias, and no cargo test depends on it. `check_xval_sync.py` is stdlib-only. Step 19 verifies this where `import numpy` genuinely fails. | Contributors touching `src/policy/**` will see the heavy workflow on their PR. Mitigated by the triage order in the auto-issue body and `xval/README.md`. |
| R8 | **The published crate misrepresents its license** (F13). | `exclude = ["/xval"]` + Step 10's negative control + C2. | If 0.1.0 is already on crates.io, that release is immutable; confirm before publishing (2.2). |
| R9 | **False-green CI**: cross-validation tests `skip` when the binary is missing, and `pytest` exits 0 when it collects nothing at all. | `OPENV2B_BIN` set explicitly; a junit-XML assertion step (5.4) that fails on any skip **and** on a collected count below the job's floor; the provenance step prints the binary sha256; the trace smoke test runs the binary before pytest. | The floor is a magic number and must be updated when experiments are added or moved between jobs; Step 15 owns it. |
| R10 | **Stale-binary validation on developer machines**: the plugin's discovery falls back to `~/openv2b/target/release/openv2b`. | CI sets `OPENV2B_BIN`; every experiment prints the resolved path and sha256 with its diff table; `RESULTS.md` records the sha256 behind each number. | A developer who ignores the printed provenance can still fool themselves. |
| R11 | **Solver-feature cost** makes X10/X11 either slow on every PR or absent from PRs entirely. | Step 6 measures it; section 5.3 gives a numeric decision rule and requires the resulting gap to be written down if the job is demoted. | If demoted: an MPC-affecting PR merges without an MPC replay until Monday. Releases stay gated. |
| R11b | **A solver failure is misread as a parity failure.** `mpc`/`oracle` exit non-zero on an infeasible or failed MILP, which surfaces to the plugin as `CalledProcessError`, indistinguishable at a glance from an unknown policy name or a real divergence. | The auto-issue triage order names it explicitly; the X10/X11 fixtures must be small and provably feasible (a fixture whose MILP can go infeasible is a bad fixture); the harness prints the binary's stderr on a non-zero exit rather than swallowing it. | A HiGHS version change could alter a degenerate optimum and move setpoints without changing the bill. Replay would still agree (it replays whatever was produced), so this is noise in the fixture, not a false red. |
| R12 | **X9 performance**: 183 EVSEs over hundreds of slots in acnportal, unmeasured. | Start with `one_day`, then a 7-day slice, and only then consider the full month. Job timeout raised to 30 minutes. A6 aborts to `one_day` only. | The full 30-day month may never be feasible; say so rather than implying it is covered. |
| R13 | **Renewed drift** from a future engine change. | C6a per PR; the path-filtered `pull_request` trigger runs the suite on engine/policy/solver changes; the tag trigger gates releases. The drift surface is now a data contract with fixed types, not 250 lines of policy semantics. | A change to a filtered path that alters semantics without altering the schema is caught only by the suite actually running, which the path filter does ensure for `src/policy/**` and `src/engine.rs`. |
| R14 | **PR merge method silently squashes the imported history** (1.5). | Explicit requirement; Step 20's `git log --graph` fails loudly. | Recoverable only by re-importing, cheap at this size. |
| R15 | **Path-filtered workflow made a required check** blocks unrelated PRs forever. | Explicit "do not mark required" in 5.2, with the always-run gate-job pattern named as the future fix. | None if the instruction is followed. |
| R16 | **Dependency-graph noise**: Dependabot permanently flags the deliberate old pins. | 2.3: leave security updates off or add an ignore list with the reason. | Cosmetic. |
| R17 | **Someone re-adds a mirror** to close a gap ("just a small EDF, to test the ordering"). | 4.4 item 3 as a written rule, C8 as the mechanical proxy, A5 as an abort criterion, and section 3.1 recorded in `DERIVATION.md` so the reasoning is available to whoever is tempted. | C8 is textual and evadable. The real control is review. |

---

## Appendix: adversarial review log

### Passes 1-3: the mirror-based draft (retained as history)

These findings shaped the document before the architecture change. Findings marked **superseded**
were resolved by removing mirrors entirely rather than by the fix recorded here.

| # | Finding | Change |
|---|---|---|
| 1.1 | `cargo package` would ship BSD-3 files inside a crate declaring MIT OR Apache-2.0 (F13), with no verification. | Added `exclude = ["/xval"]`, the packaging step, its **negative control**, the over-reach counter-check on `tools/referee.py`, and C2. |
| 1.2 | The drift tripwire was a Rust `#[test]` reading a file that `exclude` removes from the packaged crate, breaking `cargo package --verify`. | Moved to `tools/check_xval_sync.py`, a stdlib CI step. |
| 1.3 | The draft edited `LICENSE-MIT`/`LICENSE-APACHE` to add a scope note. Editing canonical license texts is itself a licensing defect. | Root license files are explicitly not to be edited; scope lives in README, CONTRIBUTING, PROVENANCE. |
| 1.4 | `CONTRIBUTING.md` said all contributions are MIT OR Apache-2.0, which becomes false for `xval/` and makes future DCO sign-offs certify the wrong license. | Added the CONTRIBUTING exception. |
| 1.5 | A path-filtered workflow marked required hangs every PR that does not touch those paths. | Explicit "do not mark required" plus the gate-job pattern (5.2, R15). |
| 1.6 | Scheduled workflows are auto-disabled after 60 days of inactivity, so "the cron will catch it" is not durable alone. | Added the `push: tags` release gate and documented the auto-disable. |
| 1.7 | `gh issue create` on a fork PR fails (read-only token), converting a parity failure into a permissions error. | Gated the auto-issue on `github.event_name == 'schedule'`. |
| 1.8 | "BSD-3 because acnportal is BSD" is legally wrong (acnportal is not vendored). A false legal claim in a provenance document is worse than none. | Section 2 states BSD-3 is a choice for attribution symmetry, not an obligation. |
| 1.9 | Linguist would flip the language bar to Python; `linguist-vendored` would assert the code is third-party, which is untrue. | Switched to `linguist-detectable=false`. |
| 1.10 | GitHub's dependency graph will permanently flag the deliberate pins. | Added R16 and the 2.3 guidance. |
| 2.1 | The charger-assignment replay was stale too (F7/F8), which is engine-level and would have survived a purely algorithmic re-sync. | **Superseded**: assignment now comes from the trace and the replay is deleted. |
| 2.2 | X4 could not detect that staleness (F9); it would have come back green and been reported as validating assignment. | **Superseded**: X4b is deleted, X4's purpose is narrowed, and assignment moves to tier 3 with its Rust coverage named. |
| 2.3 | `max_soc_kwh` and `heuristic_threshold_kw` (F10) were unknown to the bridge, inert because no fixture set them. | Retained: X1b, plus `max_soc_kwh` in both the reader and the writer. `heuristic_threshold_kw` needs no plugin consumer under replay; `write_openv2b_scenario`'s `**manifest_extra` already round-trips it. |
| 2.4 | The full edf/llf re-sync ignored `referee.py` (F16), which already gives per-PR algorithm-level differential coverage. | Rewrote the recommendation around that argument; now generalized in section 3.1. |
| 2.5 | Scoping the claim down silently dropped X2's sharp clamp-order mutation test. | **Superseded and reversed**: X2 now replays `edf` and `llf` directly, so the non-canonical emission order is back without a mirror. |
| 2.6 | "Preserving history" was overstated: filter-repo changes every SHA. | Stated the caveat; the original hashes go into the import commit message and `DERIVATION.md`, and an off-repo bundle is created first. |
| 2.7 | A GitHub PR merged with "Squash and merge" destroys the imported history with no warning. | Added 1.5 and Step 20's `git log --graph` verification. |
| 2.8 | A fresh import via `cp -r` would drag in `.venv/`, `__pycache__/`, `.pytest_cache/`, `*.egg-info/`, `xval_runs/`. | Fallback C uses `git archive`; Step 9 verifies with `find`. |
| 2.9 | The cross-validation tests skip when the binary is absent, so a misconfigured job is green while testing nothing. | Zero-skip assertion, explicit `OPENV2B_BIN`, binary-provenance step (R9). |
| 2.10 | The plugin's binary discovery falls back to the live checkout, so a developer can validate a stale binary. | Added R10. |
| 2.11 | Claims that become false were listed but not tied to a step. | Step 18 enumerates every file with an `rg` verification. |
| 2.12 | "Rust-only contributors are unaffected" was unverifiable; openv2b's CI already requires `python3`. | Step 19 states the honest version and verifies it where `import numpy` fails. |
| 2.13 | Non-vacuity lived only in README prose. | **Superseded and strengthened**: the clamp-class coverage ledger (3.8) plus a seven-mutation red-team pass. |
| 2.14 | The plugin README's "nothing in this repository is imported by it" becomes self-referential once the plugin is in that repository. | Added to Step 18. |
| 3.1 | The plan asserted the X-suite's failure modes without running anything. | Section 0.3 is labelled a prediction, Step 0 measures it, A1 aborts if reality differs. |
| 3.2 | The name-set tripwire cannot see a semantic change under an unchanged name. | **Superseded**: replaced by C6a, a typed data contract, whose analogous blind spot is narrower and is named in R3. |
| 3.3 | The independence claim is spec-level, not idea-level. | Retained and expanded into section 4's three tiers. |
| 3.4 | Reverting a merge commit poisons a future re-merge. | Rewrote the post-merge rollback around forward fixes and `git rm -r xval`. |
| 3.5 | `requirements.txt` uses `==` but not hashes. | Added `requirements.lock.txt` with `--generate-hashes`. |
| 3.6 | No degradation path for the day acnportal becomes uninstallable. | Added the explicit historical-claim path in 5.7 and R6. |
| 3.7 | Root `README.md` says v0.2-alpha while `CLAUDE.md` says v0.4-alpha. Pre-existing, unrelated. | Deliberately not in the checklist: fixing it here would blur the diff. Flag to the owner separately. |
| 3.8 | `policy-1` emits two setpoints for the same session and the engine resolves it last-wins, which a `{station_id: [kW]}` dict cannot express, leaving the clamp-order position ambiguous. | **Superseded, and resolved rather than documented**: `emission_index` emits the answer in the data (T5, T6). The question can no longer be answered wrongly. |
| 3.9 | Steps said "add a `policy-1` mirror" without saying where its semantics come from. | **Superseded**: there is no mirror. |

### Pass 4: does the replay design overclaim, and is any step vacuous?

| # | Finding | Change |
|---|---|---|
| 4.1 | The first revision said replay makes the comparison "purely an engine disagreement". False as written: `V2BChargingNetwork.update_pilots` and `BidirectionalBattery.charge` are plugin code written from openv2b's own spec by openv2b's own author, so a large part of the "independent engine" is a second implementation with the same epistemic status as `referee.py`. | Rewrote section 4 as three explicit tiers and named exactly which acnportal code is genuinely third-party (the event loop, precedence, registration, pilot validation, `charging_rates`) and which is not (the SoC recursion, the rationing passes). |
| 4.2 | The revision initially proposed replaying `power_kw` (applied). That is post-clamp, so ACN-Sim's clamps could never bind and the whole suite would have been green and near-meaningless. | Made `requested_kw` a hard requirement, and stated the failure explicitly in 3.5 so nobody "simplifies" back to applied replay. Red-team mutation (g) exists solely to make that mistake fail loudly. |
| 4.3 | With the engine self-reporting `requested_kw` and `emission_index`, "everything upstream of the request is untested" was implied but not stated, so the reader could easily believe assignment and dedup were still covered. | Added the explicit tier-3 list (4.2) and the governing input/output rule (3.2), plus R4 naming where each tier-3 item is actually covered. |
| 4.4 | Anchored mode injects an output (`soc_kwh`) back as an input, which is exactly the circularity the governing rule forbids. Left unqualified it would be the plan's own violation. | Declared it as the single exception, forbade it from carrying the claim, and added the two-mode cross-check (free-running clean iff no anchoring correction, same first divergence slot when dirty) plus A4 as an abort criterion. |
| 4.5 | "A then B" was accepted uncritically from the framing. On inspection B has **no oracle**: its trajectory diverges from openv2b's by construction whenever the engines differ, so a non-zero delta is uninterpretable, and when the engines agree it says nothing A did not. | Recommended A only. B is recorded as a conditional *portability demonstration* with a named trigger, a relabel, and a prerequisite refactor, and is explicitly barred from `RESULTS.md` as parity evidence. |
| 4.6 | B would construct the `Observation` a second time in Rust, recreating the mirror problem inside openv2b where the Python tripwire cannot see it. The first revision listed B's cost only as effort. | Added the structural objection to 3.3 and made the shared-constructor refactor a prerequisite rather than a follow-up. |
| 4.7 | The plan proposed the trace change and the fold in one PR, which muddies "openv2b remains standalone" and makes the new fields look like they exist to serve a Python package. | Split into PR0 (trace schema, justified on openv2b's own terms, with a `referee.py` consumer) and PR1 (the fold). Added the split to the checklist and to the rollback section. |

### Pass 5: hidden work, packaging, and gaps left by the staged path

| # | Finding | Change |
|---|---|---|
| 5.1 | X10/X11 need `--features solver-highs`, which the existing CI never builds (F14). The revision named the experiments without confronting the build cost. | Split the workflow into `xval` and `xval-solver`, added Step 6 to *measure* the cold and warm build, and gave section 5.3 a numeric decision rule with the resulting coverage gap written down if the job is demoted (R11). |
| 5.2 | Mapping ACN-Sim's battery capacity onto openv2b's operating ceiling silently breaks when the arrival SoC is above the ceiling, which openv2b permits (T11) and the plugin's battery rejects (T12). Would have surfaced as an opaque `ValueError` mid-experiment. | Added T11/T12 as facts, required a named refusal, and required `battery_kwh` to be carried separately as `true_capacity_kwh` for reporting. |
| 5.3 | Deleting `_replay_charger_assignment` orphans `tests/test_scenario.py`'s tests for it. Deleting them alongside would remove coverage under cover of a refactor. | Step 14 requires those tests to be **rewritten** against trace-derived assignment and the new refusals, and the step's verification greps for the deleted symbols. |
| 5.4 | X9 assumes never-connected sessions can simply be dropped, but a dropped session still passes its arrival SoC through openv2b's persistence chain (`engine.rs:143`). Dropping it from the plugin's chain would diverge on the *next* session of that vehicle, in a fixture chosen precisely because it has repeat vehicles. | Added phantom chain links to the bridge work in 3.6 and named the behavior with its line reference. |
| 5.5 | X9's cost was unestimated: 183 EVSEs over 2880 slots in a simulator nobody has profiled at that size. | Added R12, a staged fixture (day, then week, then month), a raised job timeout, and abort criterion A6 requiring the shortfall to be *stated* rather than silently dropped. |
| 5.6 | `write_openv2b_scenario` never emits a `max_soc_kwh` column, so X1b could not even be authored. The revision only mentioned the reader. | Added the writer to 3.6's bridge work. |
| 5.7 | The path filter omitted `src/state.rs` (the `Setpoint`/`Observation` contract) and `Cargo.lock` (a serializer bump could break T8's bit-exactness, which the whole CSV interface rests on). | Added both, with the reason inline. |
| 5.8 | C6a reads the Rust source, so a mismatch between what `engine.rs` declares and what `output.rs` actually writes would pass. | Added the built-binary trace schema smoke test to the job body, before pytest, with a pointer to `TRACE_CONTRACT.txt`, plus a negative control in Step 17 using a pre-PR0 binary. |
| 5.9 | Nothing prevented a mirror creeping back in later; the old tripwire was a policy-name list that no longer exists. | Added C8 (no openv2b policy name in plugin `.py` outside comments/docstrings/`REPLAYED_POLICIES.txt`), R17, and A5, and stated C8's evadability rather than overselling it. |
| 5.10 | Step 0's "record the before value" for GitHub's license detection was referenced by the post-merge step but never actually collected. | Folded the `gh api ... .license.spdx_id` capture into Step 0. |

### Pass 6: residual sweep

| # | Finding | Change |
|---|---|---|
| 6.1 | The two clamping passes differ in *pass membership* between the engines for a negative request on a unidirectional port (openv2b routes by raw sign, the plugin clamps first), even though every number agrees. A harness that compared pass membership or per-pass totals would report a spurious red. | Added T13 and the explicit instruction that the harness compares numbers only. |
| 6.2 | Adding trace columns changes the sha values recorded in `reports/OVERNIGHT_REPORT.*`. Left unmentioned, someone would later treat that as a regression. | Added Step 5 requiring PR0's message to name it, having first confirmed (T9) that no *gate* pins a trace hash. |
| 6.3 | The plan deferred `decisions.csv` without stating what the deferral costs, which is exactly the kind of quiet scope reduction this document exists to prevent. | Specified `decisions.csv`'s exact schema in 3.5 so it can be added later without redesign, and recorded the cost of deferral in 4.3 and R4. |
| 6.4 | Keeping `V2BInterface.headroom_kw` while deleting the other policy helpers was inconsistent: it combines the site cap and the DR firm level the way openv2b's *policies* do, so it is policy semantics, not plumbing. | Moved it to the deletion list in 3.6 and stated the borderline explicitly, so the line between "plumbing kept for X5" and "policy semantics deleted" is defensible rather than arbitrary. |
| 6.5 | "The trace is sufficient except for two fields" glossed over `max_soc_kwh`, which is a *plugin* gap rather than a format gap. Conflating the two would have produced an unnecessary third column. | Section 3.5's table distinguishes "format yes, plugin no" from "new field required", and only two fields are added. |
| 6.6 | The independence section did not answer the obvious reviewer question: given `tools/referee.py`, why does the ACN-Sim leg exist at all? | Added section 4.3's five points, including point 5, which argues *against* the leg's epistemic value for tier 2. A written weakness is worth more than an unwritten strength. |
| 6.7 | Nothing said what happens to the plugin's `KNOWN_ISSUES.md`, two of whose five structural limitations are resolved by this change (no `never_connected`, no MPC mirror) and one of whose stated reasons is superseded (billing). Leaving it stale would reproduce the original failure in a new file. | Step 18 enumerates the per-item disposition. |
| 6.8 | Effort was quoted only for the whole change, so the trace extension looked like part of a multi-day item and might have been bundled back into PR1. | Effort is quoted per PR, and the rollback section notes PR0 is independently keepable. |

### Pass 7: re-reading the revision cold, hunting for its own vacuous steps

This pass was run against the written revision rather than against the idea, and it found the two
most serious defects in the document.

| # | Finding | Change |
|---|---|---|
| **7.1** | **The clamp-class ledger did not do what the document claimed.** It was computed from openv2b's `trace.csv` alone, and the document asserted it would catch red-team mutation (g), a plugin that replays `power_kw` instead of `requested_kw`. It would not: the ledger reads the trace, which mutation (g) does not touch, so the ledger stays full while the plugin, fed already-feasible values, clamps nothing and every delta is exactly zero. The suite would have gone **green** on the one mutation that empties it of content, and the plan would have certified it. | Rewrote 3.8 around a **two-sided** ledger: the same detector run over openv2b's trace and over the plugin's own recorded `(requested_in, applied_out, soc_in, limits)`, with per-(slot, session) **equality** required, not merely non-emptiness. Added the `network.py` recording requirement to 3.6, added class `DIR` so T13's pass-membership asymmetry cannot produce differing labels, rewrote mutation (g)'s expected effect, and rewrote R1. |
| **7.2** | **`max_soc_kwh` has no prose description anywhere.** `docs/SPEC.md` section 2's entity list stops at capacity/target/floor/limits, section 6's validation list never mentions it, and `docs/INPUT_FORMAT.md` does not have it at all. Its only description is the doc comment on `src/scenario.rs`. Invariant 4.4.4 requires tier-2 plugin code to be written from prose, so the plan as written would have forced whoever implements X1b to read Rust, falsifying the independence claim at exactly the point X1b is meant to test, and leaving `DERIVATION.md` with no source to cite. | Added Step 1b to PR0: document the ceiling in SPEC sections 2 and 6 (including T11's rule that arrival SoC is validated against `battery_kwh`, not the ceiling) and in `INPUT_FORMAT.md`'s column table, as a stated **precondition** for X1b. |
| 7.3 | The zero-skip requirement was specified as `pytest -q -rs`, which only *reports* skips and exits 0 on zero collected tests. Two false greens dressed as a mitigation, in the risk row (R9) whose entire subject is false greens. | Replaced with a junit-XML assertion step that fails on any skip and on a collected count below a per-job floor, plus a negative control (delete an experiment file, confirm red) in Step 17. |
| 7.4 | The two jobs both ran the whole pytest suite, so the solver job would re-run X1-X9 and a single collected-count floor would fail one of the two. | Added `@pytest.mark.solver` with `--strict-markers`, `-m solver` / `-m "not solver"` partitioning, and a per-job `XVAL_MIN_TESTS`. |
| 7.5 | Red-team mutation (b) as written ("drop the `emission_index` fill so every session reports -1") also zeroes `requested_kw`, because both come from the same initialization, so it would have gone red for the wrong reason and told nothing about the ordering path. | Restated as "keep `requested_kw`, force `emission_index` to -1", and added the plugin-side contract rule `requested_kw != 0 implies emission_index >= 0` so the mutation is additionally flagged as a contract violation rather than a bare delta. |
| 7.6 | `_xval.compare` raises on any session-key-set mismatch, which X9 requires (never-connected sessions exist on one side only). The new experiment would have failed on its defining feature. | Restricted the key comparison to connected sessions and required a separate equality assertion against `sessions.csv`'s `never_connected` set. |
| 7.7 | X2 replays `edf`, but section 0.3 predicts the current fixture drives `edf` entirely into the force-charge path, which degenerates the emission order toward canonical and would silently defeat the arbitration test the experiment exists for. | X2's fixture must set `heuristic_threshold_kw` above the building load so the budget walk actually runs; stated in the X2 row with the reason. |
| 7.8 | The persistence-chain claim ("free-running derives the chain itself, so invariant 11 stays cross-validated") is conditional: the plugin chains *through* dropped sessions, and the drop set comes from openv2b (tier 3). Unqualified it overclaims. | Stated the conditionality inline in the 3.5 table. |
| 7.9 | `RESULTS.md` was specified without a mode column, so an anchored number could be published as if it were a free-running one, which is the numerically flattering direction. | The layout entry now requires the tier and the producing mode per row, with free-running explicitly the only mode that carries the claim. |
| 7.10 | A solver failure (`oracle solve failed`, missing feature, infeasible MILP) reaches the plugin as `CalledProcessError`, indistinguishable at a glance from a real divergence, in a job whose triage instructions are the first thing a reader follows. | Added it to the auto-issue triage order and added R11b, with the requirement that X10/X11 fixtures be provably feasible and that the binary's stderr be printed rather than swallowed. |
| 7.11 | "Does the staged path leave a gap?" was answered implicitly by recommending against B. The direct question deserved a direct answer. | Added the explicit comparison in 3.4: B covers *less* of tier 3 than A (it bypasses `engine::run` entirely, so it has no assignment stage at all), so declining to stage it costs a portability demonstration, not coverage. |
| 7.12 | Section 2.2 pointed at "Step 7 in section 6" for the packaging negative control; renumbering had moved it to Step 10. | Corrected. |
