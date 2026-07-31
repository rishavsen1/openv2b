# Plan: folding the ACN-Sim cross-validation plugin into openv2b

Status: **proposal, not executed.** Nothing in this document has been applied. It is written to be
executed step by step with a verification command after each step.

Scope: move `~/acnportal-v2b` (Python, BSD-3-Clause, 8 commits, never pushed, no remote) into
`~/openv2b` (Rust, public, MIT OR Apache-2.0, DCO) as `xval/acnportal-v2b/`, correct the licensing
and provenance story, wire a non-intrusive CI trigger, and repair the parity claims that went stale
when commit `a743277` replaced openv2b's heuristics with faithful OPTIMUS ports.

Out of scope: rebuilding openv2b on ACN-Sim (never), publishing the plugin to PyPI, experiment X5
(billing parity), experiment X6 (MPC canary).

---

## 0. Facts established by inspection (the plan rests on these)

Read on 2026-07-31 at openv2b `a10703f` and plugin `dcc7a0c`.

| # | Fact | Evidence |
|---|---|---|
| F1 | openv2b HEAD accepts exactly seven policies: `idle, uncontrolled, policy-0, policy-1, policy-2, edf, llf`. `edf-v2b` and `llf-v2b` **do not exist**. | `src/policy/mod.rs:23-48` (`by_name`, `POLICY_NAMES`) |
| F2 | An unknown policy name makes the binary print `unknown policy '<x>'` and return `ExitCode::FAILURE`. | `src/main.rs:167-173` |
| F3 | The plugin's runner maps five policy names, three of which are now unroutable: `uncontrolled, edf, edf-v2b, llf, llf-v2b`. | `src/acnportal_v2b/runner.py:23-30` |
| F4 | X3 runs `("edf-v2b", "llf-v2b", "llf")`; X4 runs `("edf-v2b", "llf-v2b", "uncontrolled")`. Under F2 + `subprocess.run(check=True)` those legs raise `CalledProcessError`, i.e. X3 and X4 **crash**, they do not report a diff. | `experiments/x3_v2b_discharge.py:45`, `experiments/x4_heterogeneous_ports.py:95`, `src/acnportal_v2b/openv2b.py` (`run_openv2b`) |
| F5 | openv2b's `Uncontrolled` was **not** touched by the port commit: still `need = max(target - soc, 0)`, `min(need/eta/dt, max_charge_kw)`. | `src/policy/heuristics.rs:44-68`, `git show a743277 --stat` |
| F6 | openv2b HEAD's charger assignment prefers a **bidirectional port for every car**, ties by lowest charger id, and **drops permanently** any car that finds no vacancy. | `src/engine.rs:167-193` (`min_by_key(\|&c\| (!bidirectional, c))`) |
| F7 | The plugin's replay instead matches port capability to vehicle capability (`chargers[c].bidirectional == wants_bidi`) and *retries* a waiting car on later slots. **Stale vs F6.** | `src/acnportal_v2b/scenario.py:250-271` |
| F8 | X4's fixture does not detect F7: its bidirectional-capable vehicle (id 0) also arrives first (slot 2), so both rules hand it charger 1. **X4 is vacuous with respect to the assignment change.** | `experiments/x4_heterogeneous_ports.py:59-62` + F6/F7 |
| F9 | openv2b HEAD's `Vehicle` gained `max_soc_kwh` (operating ceiling, distinct from `battery_kwh`) and the manifest gained `heuristic_threshold_kw`. The plugin's scenario reader knows **neither**. | `src/scenario.rs:101-106`, `src/scenario.rs:35-39`; `src/acnportal_v2b/scenario.py:320-326` |
| F10 | Missing CSV columns still deserialize (serde defaults; the shipped `examples/one_day/vehicles.csv` has no `max_soc_kwh` and CI is green), so the plugin's writer is **format-compatible** with HEAD even though it is semantically incomplete. | `src/scenario.rs:240-247`, `examples/one_day/vehicles.csv` header |
| F11 | Both repos sign off every commit (DCO). | `git log --format='%(trailers:key=Signed-off-by)'` on both |
| F12 | openv2b's `Cargo.toml` has **no `exclude`/`include`**, so `cargo package` would ship every git-tracked file, including a folded `xval/`. | `Cargo.toml` |
| F13 | openv2b CI already requires `python3` (stdlib only, for `tools/referee.py` and `tools/convert_optimus.py`) and already does `cargo build --release`. | `.github/workflows/ci.yml` |
| F14 | The plugin's git history is 8 commits, 216 KiB of objects, ~3.8 kLOC, single author. | `git count-objects -vH`, `wc -l` |
| F15 | openv2b already contains an independent Python re-implementation of every heuristic (`tools/referee.py`) that runs on **every PR**. | `.github/workflows/ci.yml`, `CLAUDE.md` |

**Predicted behavior of the X-suite against HEAD, unchanged** (to be confirmed in Step 0, not
assumed):

| experiment | leg | prediction | why |
|---|---|---|---|
| X1 | `uncontrolled` x {eta 1.0, 0.92} | **green** | F5, F10; 1 EV : 1 charger so assignment is trivial |
| X2 | `uncontrolled` | **green** | all six ports unidirectional, so F6 and F7 agree; `Uncontrolled` unchanged |
| X2 | `edf` | **red, large** | HEAD `edf` is a threshold-budget scheduler; with no `heuristic_threshold_kw` the fallback threshold is `0.8 x 18 = 14.4` kW, which the 18 kW midday building load already exceeds, so the budget walk breaks at the top of the loop and only the force-charge path fires |
| X3 | `edf-v2b`, `llf-v2b` | **crash** | F2 + F4 |
| X3 | `llf` | **red** | same class as X2/`edf` |
| X4 | `edf-v2b`, `llf-v2b` | **crash** | F2 + F4 |
| X4 | `uncontrolled` | **green (vacuously)** | F8: the fixture cannot see the assignment-rule change |

So: the engine/physics claim survives; the algorithm claim does not; and one of the four
experiments is green for the wrong reason. That shape drives the recommendation in section 5.

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
      KNOWN_ISSUES.md
      DERIVATION.md                <- new: per mirrored policy, the prose source it was written from
      RESULTS.md                   <- new: the parity table, anchored to an openv2b commit + a run URL
      MIRRORED_POLICIES.txt        <- new: the drift tripwire's fixture
      pyproject.toml, requirements.txt, requirements.lock.txt
      .gitignore                   (kept: scopes .venv/, __pycache__/, xval_runs/ locally)
      src/acnportal_v2b/*.py
      experiments/*.py
      tests/*.py
```

Why two levels rather than putting the package directly in `xval/`:

- the Python package root stays self-contained, so `xval/acnportal-v2b/LICENSE` unambiguously
  governs a directory whose boundary is obvious, and
  `pip install "git+https://github.com/rishavsen1/openv2b#subdirectory=xval/acnportal-v2b"` works
  unchanged, and a later PyPI publish is a `working-directory:` away;
- `xval/` is left free for a second cross-validator (`xval/acndata/`, a future SAA reference) without
  reshuffling.

`xval/` is **not** a cargo workspace member. openv2b's `Cargo.toml` declares a single package with no
`[workspace] members`, and cargo auto-discovers targets only under `src/bin`, `tests`, `examples`,
`benches` at the package root. Nothing in `xval/` is compiled, linted, or formatted by cargo.

### 1.2 History: recommendation

**Recommended: Option A, path-rewritten history merge.** Clone the plugin to a throwaway location,
rewrite every historical path under `xval/acnportal-v2b/`, then merge that rewritten history into an
openv2b branch with `--allow-unrelated-histories`.

Rationale:

- The 8 commits are *evidence for the independence claim* (section 3): they show the plugin's model
  classes, network guard, and algorithm mirrors were written before any parity run, against
  `docs/SPEC.md`, in a repo that never contained a line of Rust. A squashed import throws that
  away and replaces it with an assertion.
- All 8 commits are DCO-signed by the same person who owns openv2b (F11), so the sign-off chain
  carries over intact and no relicensing consent problem arises.
- Path rewriting (rather than plain `git subtree add`) is what makes the history *usable*:
  after a subtree add, the historical commits still record paths like `src/acnportal_v2b/models.py`,
  so `git log -- xval/acnportal-v2b/` shows only the merge and `git blame` stops at it.

Honest caveats to record in the import commit message:

- Rewriting changes every commit hash. "History preserved" means messages, authorship, dates,
  sign-offs, and diffs are preserved; the 8 original SHAs are not. Record them in
  `xval/acnportal-v2b/DERIVATION.md` and keep an off-repo `git bundle` of the pre-import repo.
- `git filter-repo` is an external tool (`pipx install git-filter-repo`). Fallbacks are given below.

**Rejected: Option B, `git subtree add`.** Preserves commits but not paths (see above). Use only if
filter-repo cannot be installed and the owner still wants the commits.

**Rejected: Option C, fresh squashed import.** Cheapest, but discards the independence evidence and
the per-commit sign-offs for no benefit given F11. Keep it as the abort-path fallback only.

### 1.3 Exact commands (Option A)

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
git commit -s        # message: see 1.4
git remote remove xval-import

# --- 3. Everything else (hygiene, docs, CI, re-sync) as ordinary signed commits on xval-fold.
```

Fallback B (no filter-repo, commits kept, paths not rewritten):

```bash
cd /home/rishav/openv2b && git switch -c xval-fold
git remote add xval-import /home/rishav/acnportal-v2b
git fetch xval-import
git subtree add --prefix=xval/acnportal-v2b xval-import main
git remote remove xval-import
```

Fallback C (fresh import; note `git archive` ships **tracked files only**, so `.venv/`,
`__pycache__/`, `.pytest_cache/`, `*.egg-info/` and `xval_runs/` cannot leak in — a plain `cp -r`
would leak all five):

```bash
cd /home/rishav/openv2b && git switch -c xval-fold
mkdir -p xval/acnportal-v2b
git -C /home/rishav/acnportal-v2b archive main | tar -x -C xval/acnportal-v2b
git add xval && git commit -s
```

### 1.4 Merge-strategy requirement (easy to get wrong)

If this lands through a GitHub pull request, PR1 **must be merged with "Create a merge commit"**.
"Squash and merge" collapses the imported history into one commit and silently defeats Option A;
"Rebase and merge" refuses or flattens a merge commit. If the repository's default merge method is
squash, either change it for this PR or push `xval-fold` to `main` directly.

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

The plugin is BSD-3-Clause **by its own choice**, for attribution symmetry with acnportal. It is not
a derivative work of acnportal in the copyright sense: no acnportal source is copied or patched;
acnportal is a pip dependency resolved at runtime. That distinction must be stated precisely,
because "BSD because acnportal is BSD" is a claim about obligation, and it is false here. Getting it
right matters: mixing permissive licenses is trivially legal, so the only real risk is
*misdescription*.

### 2.1 What each artifact must say

| Artifact | Required content |
|---|---|
| `xval/acnportal-v2b/LICENSE` | Unchanged, verbatim BSD-3-Clause with the existing acnportal attribution preamble. Never edited, never relicensed. |
| `xval/acnportal-v2b/README.md` | First section after the title: "**License: BSD-3-Clause** (see `LICENSE`), unlike the rest of this repository, which is MIT OR Apache-2.0. This directory is not compiled into, linked by, or packaged with the `openv2b` crate." |
| `xval/README.md` (new) | One screen: what `xval/` is for, that it is Python and optional, that Rust-only contributors need none of it, and the per-directory license rule. |
| Root `README.md`, "Provenance and license" | Add: "Everything in this repository is `MIT OR Apache-2.0` **except `xval/acnportal-v2b/`**, which is BSD-3-Clause (its own `LICENSE`). The published `openv2b` crate excludes `xval/` entirely." |
| Root `LICENSE-MIT`, `LICENSE-APACHE` | **Do not edit.** They are the license texts, not scope statements. Scope belongs in README/PROVENANCE. Editing a canonical license text is itself a licensing defect. |
| `CONTRIBUTING.md` | Amend "All contributions are accepted under `MIT OR Apache-2.0`" to except `xval/acnportal-v2b/`, whose contributions are accepted under BSD-3-Clause. Without this the DCO sign-off on future xval commits certifies the wrong license. |
| `docs/PROVENANCE.md` | Section 1 item 3 currently says "No code has been vendored from them to date". Still true (acnportal source is *not* vendored) but now misleading. Add a subsection "Third-party-licensed subdirectories" naming `xval/acnportal-v2b/`, its license, why (attribution symmetry, not obligation), and the rule that BSD-3 content must never move into `src/`, `tests/`, `examples/`, or `tools/`. |
| Every `.py` file under `xval/acnportal-v2b/` | Add `# SPDX-License-Identifier: BSD-3-Clause` as the first line. 13 files. This is the cheap insurance: if a file is ever copied out of the directory, its license travels with it. |

### 2.2 Cargo packaging

F12 is a real defect once the fold lands: `cargo package` collects git-tracked files, so the `.crate`
would contain BSD-3-Clause Python under a manifest declaring `license = "MIT OR Apache-2.0"`. That is
a misrepresentation on crates.io, and it also bloats the crate with files no consumer can use.

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
  (`cargo search openv2b`, or check https://crates.io/crates/openv2b): if 0.1.0 *is* already
  published it cannot be replaced, and the exclude only takes effect from the next version.
- The exclusion must be verified **with a negative control**; see Step 4 in section 6.

### 2.3 Cosmetic and tooling side effects

- **GitHub Linguist** will start reporting a large Python share. Add `.gitattributes`:
  `xval/** linguist-detectable=false`. Prefer `linguist-detectable=false` over `linguist-vendored`:
  the code is not vendored and marking it so would be a small untruth in exactly the document
  (provenance) where untruths are expensive.
- **Dependency graph / Dependabot**: a public repo's dependency graph will now ingest
  `requirements.txt` and permanently flag `numpy<2`, `pandas<2`, `setuptools<81`. These pins are
  deliberate (acnportal 0.3.3 predates numpy 2 and imports `pkg_resources`). Either leave Dependabot
  security updates off, or add `.github/dependabot.yml` ignoring the three packages with the reason
  in a comment. Do not "fix" the pins; that breaks the plugin.
- **GitHub license detection** reads root-level license files only; the nested `LICENSE` should not
  change the repository's detected license. Verify after push (section 6, Step 12).

---

## 3. Preserving the independence claim

The credibility argument in the root README is: *"because the two simulators share no code,
agreement between them is evidence of correctness rather than of a shared bug."* Co-locating them in
one repository does not weaken that claim, but it removes the physical barrier that used to enforce
it. The barrier must be replaced by written invariants plus mechanical checks.

### 3.1 What must be true after the move (and stay true)

1. **No build-time or run-time coupling.** `src/**` contains no reference to `xval`, `acnportal`, or
   Python beyond the existing stdlib tools. `cargo build`, `cargo test`, `cargo clippy` never touch
   `xval/`. The published crate excludes it.
2. **The plugin drives openv2b only as a black box.** `xval/acnportal-v2b/src/acnportal_v2b/openv2b.py`
   invokes the release binary as a subprocess and parses `slots.csv` / `sessions.csv` /
   `summary.json`. No Python file under `xval/` may read, import, parse, or transcribe anything under
   `openv2b/src/`.
3. **The mirrors are written from prose, not from Rust.** Each mirrored policy in the plugin is
   derived from a *written specification* (`docs/SPEC.md` section 4, `docs/OPTIMUS_PORT.md`'s
   ported-semantics prose), not by reading `src/policy/heuristics.rs`. Recorded per policy, with the
   date and the exact section, in `xval/acnportal-v2b/DERIVATION.md`.
4. **No expectation flows the other way.** No file under `tests/`, `examples/`, `tools/`, or `src/`
   may be generated by, copied from, or hand-transcribed out of the plugin. openv2b's fixtures stay
   synthetic or hand-computed (`docs/PROVENANCE.md` already forbids foreign-tool output as fixtures;
   this extends the rule to the in-repo plugin).
5. **The claim is scoped and versioned.** The README states *which* legs agree, *at which openv2b
   commit*, and *with which policies mirrored*. A bare "max |delta| = 0.0" with no scope is what went
   stale the first time.

### 3.2 What would falsify it

Any one of these, and the claim must be withdrawn from the README the same day:

- someone imports openv2b logic into the plugin, or transcribes `heuristics.rs` into Python to close
  a parity gap;
- someone vendors the plugin's expected values into Rust fixtures ("golden" arrays copied from a
  diff table);
- the plugin starts calling openv2b as a *library* (a PyO3 binding, a shared FFI object) rather than
  a subprocess;
- openv2b grows a `build.rs`, cargo alias, or test that shells into `xval/`;
- a parity gap is closed by loosening `_xval.TOLERANCE` instead of by finding the divergence.

Note the honest limitation to write down rather than paper over: **the independence is
spec-level, not idea-level.** Both implementations are written against openv2b's `docs/SPEC.md`, and
since `a743277` that spec itself encodes the reference simulator's algorithms. So agreement rules out
independent-implementation bugs (unit errors, ordering, clamping, accounting, off-by-one window
conventions) but does **not** rule out a shared misreading of the spec. That is precisely the same
posture openv2b already takes toward `tools/referee.py`, and it is worth one sentence in the README
so a reviewer does not have to find the hole themselves.

### 3.3 Mechanical checks

`tools/check_xval_sync.py` (new, stdlib only, runs in the existing cheap CI job on every PR) asserts:

| # | Assertion | Kills the mutation |
|---|---|---|
| C1 | `xval/acnportal-v2b/LICENSE` exists and contains `BSD 3-Clause` | someone deletes or relicenses it |
| C2 | Root `Cargo.toml` `[package]` contains an `exclude` entry matching `xval` | someone drops the exclude while editing the manifest |
| C3 | Root `README.md` contains the BSD-3 exception sentence | the license story rots out of the front page |
| C4 | No file under `src/`, `tests/`, `examples/` mentions `acnportal` or `xval` | reverse coupling / vendored expectations |
| C5 | No file under `xval/**/*.py` references a path under openv2b's `src/` or a `.rs` file | Rust transcription |
| C6 | The set of openv2b policy names claimed as mirrored in `xval/acnportal-v2b/MIRRORED_POLICIES.txt` is a subset of `POLICY_NAMES` parsed out of `src/policy/mod.rs` | **the exact drift that happened**: a policy is renamed or deleted and the mirror silently keeps claiming it |
| C7 | Every `.py` under `xval/` starts with the SPDX line | a new file lands without a license marker |

C6 is the tripwire that would have caught `a743277` the day it landed. It is deliberately a Python
script run as a CI step rather than a Rust `#[test]`: a Rust test would have to read a file that
`exclude` removes from the packaged crate, which is a trap for anyone who later runs
`cargo package --verify` or vendors the crate.

C6 detects *renaming and deletion*. It cannot detect a policy whose **semantics** changed under the
same name (which is the other half of what `a743277` did). That half is covered by actually running
the parity suite, which is section 4's job, plus the release gate in Step 5 of section 6.

---

## 4. CI design

### 4.1 Constraints

- The heavy job needs CPython 3.10 + acnportal 0.3.3 + numpy 1.26 / pandas 1.5 / setuptools 80,
  which transitively pulls matplotlib, scipy, and scikit-learn: roughly 2-4 minutes of installation.
- It also needs `cargo build --release`, ~2 minutes cold, well under a minute with
  `Swatinem/rust-cache` (already in use).
- The X-suite itself runs in seconds.
- openv2b's existing CI is a fast Rust-plus-stdlib-Python job that contributors expect to stay fast.

So: **two workflows, not one job.** The cheap invariants go in the existing `ci.yml`; the heavy
parity run gets its own `xval.yml` with narrow triggers.

### 4.2 Triggers

```yaml
# .github/workflows/xval.yml
name: Cross-validation (ACN-Sim)

on:
  workflow_dispatch:            # on demand, always available
  schedule:
    - cron: "17 6 * * 1"        # Mondays 06:17 UTC: catches dependency rot, not code churn
  push:
    tags: ["v*"]                # release gate: a tag must not ship a stale parity claim
  pull_request:
    paths:                      # only PRs that can actually break parity pay for it
      - "src/engine.rs"
      - "src/scenario.rs"
      - "src/output.rs"
      - "src/policy/**"
      - "xval/**"
      - ".github/workflows/xval.yml"

concurrency:
  group: xval-${{ github.ref }}
  cancel-in-progress: true
```

Trigger rationale, one line each:

- `pull_request` + `paths`: the only PRs that can break parity are the ones touching the engine, the
  I/O contract, the policies, or the plugin. Everything else stays on the fast path.
- `schedule`: parity can break with no code change at all (a yanked wheel, a new setuptools). Weekly
  is enough; the failure mode is slow, not urgent.
- `push: tags`: a release is the moment the README's parity claim gets read by strangers.
- `workflow_dispatch`: needed for the red-team verification in section 6 and for one-off debugging.

**Do not mark this workflow a required status check.** A path-filtered workflow that does not run
produces no check at all, and a required check that never arrives blocks every unrelated PR forever.
If a required gate is wanted later, the standard pattern is a second always-running job that
`needs:` the filtered one and passes when it is skipped; that complexity is not justified yet.

`schedule` triggers are disabled by GitHub after 60 days of repository inactivity (with an email to
the owner). The tag trigger does not rot, which is why both exist.

### 4.3 Job body

```yaml
jobs:
  xval:
    runs-on: ubuntu-latest
    timeout-minutes: 25
    permissions:
      contents: read
      issues: write             # only used by the scheduled-failure reporter below
    steps:
      - uses: actions/checkout@v4

      # --- the binary under test is built from THIS commit, never downloaded ---
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Build openv2b (default features; the X-suite needs no solver)
        run: cargo build --release
      - name: Record binary provenance
        run: |
          echo "openv2b commit: $(git rev-parse HEAD)"          | tee -a "$GITHUB_STEP_SUMMARY"
          echo "binary sha256:  $(sha256sum target/release/openv2b | cut -d' ' -f1)" \
                                                                 | tee -a "$GITHUB_STEP_SUMMARY"

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
          python -m pytest -q 2>&1 | tee -a "$GITHUB_STEP_SUMMARY"

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

          Triage order: (1) did the install step fail? then it is dependency rot, not parity.
          (2) did an experiment raise \`CalledProcessError\`? then a mirrored policy name no longer
          exists in openv2b (see tools/check_xval_sync.py C6). (3) otherwise read the diff table's
          first divergent slot." \
            --label xval
```

### 4.4 How failures surface

| trigger | surfacing |
|---|---|
| `pull_request` (path-filtered) | ordinary red check on the PR + the diff table in the job summary |
| `workflow_dispatch` | red run + job summary |
| `schedule` | red run + **an auto-filed issue**, because nobody watches a cron run |
| `push: tags` | red run on the tag; the release must not be published until it is green |

The auto-issue is gated on `github.event_name == 'schedule'` on purpose: a `pull_request` run from a
fork has a read-only token and `gh issue create` would fail, turning a parity failure into a
confusing permissions error.

The diff tables go to `$GITHUB_STEP_SUMMARY` (readable without expanding logs) and `xval_runs/` is
uploaded as an artifact so a failure can be reproduced locally from the exact scenario directories.

### 4.5 Cheap per-PR additions to `ci.yml`

Append one step to the existing `test` job (no new dependencies, `python3` is already required):

```yaml
      - name: xval sync + license invariants
        run: python3 tools/check_xval_sync.py
```

This is the piece that runs on **every** PR and is the actual anti-drift mechanism (section 3.3).

### 4.6 Pinning against dependency rot

`requirements.txt` already carries exact `==` pins, but `==` does not protect against a re-uploaded
or yanked artifact. Generate a hash-pinned lock once and commit it:

```bash
cd xval/acnportal-v2b
uv pip compile --generate-hashes --python-version 3.10 requirements.txt -o requirements.lock.txt
# or: pip-compile --generate-hashes --output-file requirements.lock.txt requirements.txt
```

`requirements.txt` stays as the human-readable rationale document (it explains *why* each bound
exists); `requirements.lock.txt` is what CI installs. Both are committed; `check_xval_sync.py` does
not need to compare them (that would be a maintenance tax with little payoff) but the lock file must
be regenerated whenever `requirements.txt` changes, and the release gate re-runs the whole install
from scratch, which is where a drifted lock would show up.

Documented degradation path, so the project is honest when this eventually breaks: Python 3.10 hits
end-of-life in October 2026 and acnportal 0.3.3 has had no upstream release since 2023-11-21. If the
environment becomes uninstallable, the parity claim does **not** silently rot: it becomes a
*historical* claim, `RESULTS.md` records the last green openv2b commit and run URL, the README says
so in the past tense, and `xval.yml`'s schedule is disabled deliberately rather than left red.

---

## 5. The re-sync the move implies

### 5.1 The choice

**Option A: re-sync the mirrors to the ported policies.** Reimplement, in Python, the
threshold-budget scheduler: parquet-or-fallback threshold with the monotone ratchet, strict
peak-TOU/ceiling eligibility, the EDF deadline-pressure key with IEEE inf/NaN ordering, the LLF raw
`time_left` key, the budget walk's exact clip arithmetic against the already-decremented capacity,
clip-to-charger-max, taper-last anchored at 90% of *true* capacity, the served predicate, signed
needs as the discharge channel, and the 1-hour force-charge bypass that feeds the ratchet.

- Coverage: highest. Keeps X2/X3/X4 as algorithm-level parity tests.
- Cost: ~250 lines of intricate Python, plus new bridge state (a per-episode ratchet), plus
  `heuristic_threshold_kw` and `max_soc_kwh` support in the scenario reader.
- **Recurring** cost: every future change to the ports re-breaks it. That is the loop this fold is
  supposed to end, and it re-creates it in a place where the tripwire (C6) cannot see it, because a
  semantic change under an unchanged name is invisible to a name-set check.
- Independence risk: highest. The honest way to write it is from `docs/OPTIMUS_PORT.md`'s prose, but
  the prose is deliberately terse ("the guard compares against the already-decremented capacity;
  copied verbatim, do not fix") and the shortest path to parity is to open `heuristics.rs`. The
  moment someone does, agreement stops being evidence.
- Marginal value: **low**, and this is the decisive point. openv2b already ships
  `tools/referee.py`, an independent Python re-simulation of every heuristic that must agree
  slot-exactly and **runs on every PR** (F15). Algorithm-level differential coverage already exists,
  cheaper and more often. What ACN-Sim uniquely provides is a *foreign engine*: unmodified upstream
  event loop, model classes, network, and interface. The referee cannot provide that, because it is
  openv2b's own arithmetic re-typed.

**Option B (recommended): scope the parity claim to engine and physics, with two small mirrors.**

- Keep `uncontrolled` (unchanged at HEAD per F5, eight lines of arithmetic, essentially
  drift-proof). It is also adversarial by construction: it ignores the site cap entirely, so the
  *engine's* clamp and rationing order are what bind, which is exactly the property under test.
- Add a `policy-1` mirror. It is the cheapest ported policy that exercises the **discharge** path
  (charge to ceiling off-peak/super-off-peak, discharge above-target cars at peak, the discharge
  pass overwriting the charge pass), so the V2B physics, the no-export guard, asymmetric
  efficiencies, and export accounting all stay covered without the threshold-budget machinery.
- **Delete** the `EDF`, `LLF`, and the `v2b=True` overlay from `algorithms.py`, and remove
  `edf`, `edf-v2b`, `llf`, `llf-v2b` from `runner.POLICIES`. Delete rather than rename: leaving a
  stale mirror in place under a live openv2b name is exactly how the false claim was manufactured,
  and openv2b's own port commit set the precedent ("DELETED, not renamed").
- Record the reduced scope in `MIRRORED_POLICIES.txt` (`uncontrolled`, `policy-1`), which C6 then
  enforces.

Coverage honestly lost under Option B, stated rather than hidden:

1. **Algorithm-level parity for `edf`/`llf`** is no longer cross-simulator. Mitigation: it remains
   covered per-PR by `tools/referee.py` and by the RISHAV_WEEK bill-parity ledger in
   `docs/OPTIMUS_PORT.md`.
2. **Emission-order arbitration under a non-canonical order.** X2's sharp mutation test (deleting
   `set_clamp_order` diverges the `edf` leg by 22 kWh) depended on a policy whose emission order
   differs from the canonical `(arrival_slot, vehicle_id)` order. `uncontrolled` emits in canonical
   order, so a clamp-order bug would be invisible. Mitigations: (a) `policy-1`'s two-pass structure
   gives a second, different emission order; (b) the property is separately pinned in Rust by the R2
   audit tests (`tests/audit_r2.rs`); (c) Step 9's red-team verification requires demonstrating a
   *new* mutation that the reduced X2 still kills, and if none exists the step fails and Option A is
   reconsidered for `edf` alone. This must be demonstrated, not asserted.

**Recommendation: Option B**, with Option A available later as a separately-scoped experiment
(`X2b`, `edf` only) if a reviewer specifically demands cross-simulator algorithm parity for the
paper's headline policy. Take that decision with the numbers from Step 0 in hand.

### 5.2 Re-sync work required under **either** option

These are not algorithm work and are easy to miss:

1. **`_replay_charger_assignment` is stale (F6/F7).** HEAD prefers a bidirectional port for *every*
   car (ties: lowest charger id) and drops an unassignable car *permanently* rather than retrying it
   on later slots. Rewrite both rules. Keep the refusal for queueing, but fix its message: at HEAD a
   car never "waits", it is dropped, so the refusal reason is "openv2b would report this session
   `never_connected`, which acnportal cannot represent".
2. **X4 is vacuous with respect to that rule (F8).** Add `X4b`: same port fleet, but a
   *charge-only* vehicle arrives **before** the V2B-capable one. Under HEAD the charge-only car takes
   the 8 kW bidirectional port and the V2B car gets a 20 kW unidirectional port with zero export;
   under the plugin's current rule the opposite. The two rules must produce visibly different
   numbers, or the fixture is not doing its job.
3. **`max_soc_kwh` (F9)** is unknown to the bridge, which clamps charging and the persistence chain
   at `battery_kwh`. Add it, and add `X1b`: one EV with `max_soc_kwh` strictly below `battery_kwh`
   and a target above the ceiling, so the engine ceiling binds and a bridge that ignores it diverges.
4. **`heuristic_threshold_kw` (F9)** must at minimum be round-tripped by
   `write_openv2b_scenario`/`load_openv2b_scenario` so fixtures can pin it. Under Option B nothing
   consumes it; under Option A the mirror seeds its budget from it.
5. **Non-vacuity assertions promoted from prose to code.** The plugin's README argues non-vacuity in
   English ("X3 exports 24 kWh", "X2 vehicle 5 draws 9.25 kWh only because the force-charge fallback
   fires"). Move each into an `assert` inside the experiment so a fixture that silently stops
   exercising its guard fails instead of passing. Minimum set: total EV energy > 0 in every
   experiment; the site cap binds in >= 1 slot in X2; total exported energy > 0 in X3/X4; the engine
   ceiling binds in X1b; the two assignment rules differ in X4b.
6. **Anchor the claim.** `RESULTS.md` records, for each green run: the openv2b commit SHA, the
   binary sha256, the acnportal version, the date, the CI run URL, and the per-experiment max
   |delta|. The root README links to it rather than restating numbers.
7. **`policy-1` emits two setpoints per session, and the engine resolves the collision.** Its
   charge pass and discharge pass both emit for an above-target car at peak TOU, and
   `src/policy/heuristics.rs:209-215` documents that the later setpoint overrides the earlier one
   ("exactly like the reference's second loop overwriting the action slot"). The plugin's
   `schedule()` returns a `{station_id: [kW]}` dict, which cannot express a duplicate, so the mirror
   must replicate last-write-wins explicitly. **Open question to settle in Step 8, not to guess:**
   which position the de-duplicated session takes in the emission order that
   `interface.set_clamp_order` feeds the engine's rationing -- its first appearance or its last.
   Read openv2b's engine resolution, write the answer into `DERIVATION.md`, and pin it with a
   fixture where the two orders give different numbers (a binding site cap with an above-target
   donor at peak). This detail also partly restores the emission-order coverage lost with the `edf`
   leg (5.1, loss 2), because `policy-1`'s two-pass order is not the canonical session order.

### 5.3 X1-X4 after the fold: what must be re-run and what "green" means

| ID | scope after fold | policies | green means |
|---|---|---|---|
| **X1** unit/time-convention mapping | unchanged | `uncontrolled` x {eta=1.0, eta_c=0.92} | all 9 compared quantities <= 1e-6; delivered energy > 0 (non-vacuity). Run this **first**: it is the I/O-contract canary. Red here means columns, units, or CSV schema moved, not physics. |
| **X1b** operating-ceiling parity (new) | new | `uncontrolled` | <= 1e-6, **and** the departure SoC equals `max_soc_kwh` (< `battery_kwh`), proving the ceiling bound |
| **X2** contention, site cap, engine arbitration | `edf` leg **removed** (Option B) | `uncontrolled` | <= 1e-6; the site cap binds in >= 1 slot; **and** a demonstrated mutation (reverse the clamp order in the bridge) makes it red. If no such mutation exists, this experiment is not testing arbitration and the step fails. |
| **X3** discharge, DR window, banking | `edf-v2b`/`llf-v2b`/`llf` legs **replaced** | `policy-1` | <= 1e-6 on signed power, net load, exported kWh, and departure SoC; exported energy > 0; net load >= 0 in both simulators for every slot (no-export guard); the `(start, end]` window boundary slots agree exactly |
| **X4** heterogeneous ports, assignment replay | `edf-v2b`/`llf-v2b` legs **replaced** | `uncontrolled`, `policy-1` | <= 1e-6; the V2B donor is assigned the bidirectional port and exports > 0 |
| **X4b** assignment-rule falsifier (new) | new | `uncontrolled` | <= 1e-6 under HEAD's rule, **and** demonstrably red if the bridge is reverted to capability-matching. Without the second half this is another vacuous green. |
| **X5** itemized-bill parity | still **not implemented** | - | out of scope; keep it listed as the largest open gap in `KNOWN_ISSUES.md`, unchanged |
| **X6** MPC information-loss canary | still **not implemented** | - | out of scope; unchanged |

"Green" for the suite as a whole additionally requires: `pytest` exits 0 with **no skips** in CI (a
skipped cross-validation test because the binary was not found is the classic false green; the CI
job must assert that the four/six experiments actually ran, e.g. `pytest -q --no-header -rs` plus a
check that the skip count is zero).

---

## 6. Execution checklist, with a verification that could fail

Each step lists the command that proves it worked. Where a check could pass for the wrong reason, a
**negative control** is specified: deliberately break the thing, confirm the check goes red, restore.

| # | Step | Verification (and negative control) |
|---|---|---|
| **0** | **Measure before touching anything.** In the existing plugin repo, with the existing binary, run all four experiments and capture the output verbatim to `/tmp/xval_baseline.txt`. | The captured output matches the predictions table in section 0, or the plan's section 5 is revised to fit reality *before* any commit. Specifically: X1 green, X2/`uncontrolled` green, X2/`edf` red, X3 and X4 crash on `edf-v2b`. If X1 is red, stop: the I/O contract moved and that is a different problem. |
| 1 | Bundle + hash-list the plugin repo (section 1.3 safety rails). | `git bundle verify ~/acnportal-v2b-preimport.bundle` prints "The bundle is valid"; the hash file has 8 lines. |
| 2 | Rewrite the clone and import onto `xval-fold` (section 1.3). | `git log --oneline -- xval/acnportal-v2b \| wc -l` >= 9 (8 rewritten + merge). `git show --stat HEAD~1 \| head` shows `xval/acnportal-v2b/...` paths. **Negative control:** on a plain `git subtree add` the same `git log --` count is 1; if you get 1, the rewrite did not happen. |
| 3 | Confirm nothing untracked leaked in. | `git status --porcelain` empty; `find xval -name '__pycache__' -o -name '.venv' -o -name '*.egg-info' -o -name 'xval_runs'` prints nothing. |
| 4 | Add `exclude = ["/xval"]` to `Cargo.toml`. | `cargo package --list --allow-dirty \| grep -c '^xval/'` prints `0`. **Negative control:** comment the `exclude` out, re-run, confirm it prints a number > 20, restore. Also `cargo package --list --allow-dirty \| grep -c 'tools/referee.py'` must still print `1` (proves the exclude did not over-reach). |
| 5 | Licensing artifacts: SPDX headers (13 files), `xval/README.md`, plugin README header, root README exception sentence, `CONTRIBUTING.md` exception, `docs/PROVENANCE.md` subsection, `.gitattributes`. | `rg -c 'SPDX-License-Identifier: BSD-3-Clause' xval/acnportal-v2b --glob '*.py' \| wc -l` equals the count from `fd -e py . xval \| wc -l`; `rg -q 'xval/acnportal-v2b' README.md CONTRIBUTING.md docs/PROVENANCE.md` succeeds for all three. |
| 6 | Write `tools/check_xval_sync.py` (checks C1-C7) and wire it into `ci.yml`. | `python3 tools/check_xval_sync.py` exits 0. **Negative controls, one per check, all restored afterwards:** rename `xval/acnportal-v2b/LICENSE` (C1 red); delete the `exclude` line (C2 red); add `edf` to `MIRRORED_POLICIES.txt` (C6 red); drop an SPDX header (C7 red). A check with no demonstrated red is a check that does not exist. |
| 7 | Fix the bridge: assignment rule (5.2.1), `max_soc_kwh` (5.2.3), `heuristic_threshold_kw` round-trip (5.2.4). Update the refusal message. | `pytest xval/acnportal-v2b/tests -q` green; the new unit test for the assignment rule fails if the rule is reverted. |
| 8 | Re-scope the mirrors (Option B): delete `EDF`/`LLF`/`v2b` overlay, add `policy-1`, update `runner.POLICIES` and `MIRRORED_POLICIES.txt`, write `DERIVATION.md`. | `python3 tools/check_xval_sync.py` still green (C6 now compares `{uncontrolled, policy-1}` against `POLICY_NAMES`); `rg -n 'edf\|llf' xval/acnportal-v2b/src` returns nothing. |
| 9 | Rewrite the experiments: X1 unchanged, add X1b, X2 reduced to `uncontrolled`, X3 to `policy-1`, X4 to `uncontrolled`+`policy-1`, add X4b. Add every non-vacuity assertion from 5.2.5. | Full suite green. **Then the red-team pass, which is the real verification:** (a) reverse the bridge's clamp order -> X2 red; (b) revert the assignment rule to capability-matching -> X4b red; (c) ignore `max_soc_kwh` -> X1b red; (d) drop the `(start, end]` `+1` in `DrEvent.contains` -> X3 red. Any mutation that does **not** produce a red identifies a vacuous experiment, and that experiment must be fixed before proceeding. |
| 10 | Generate `requirements.lock.txt` (hash-pinned). | In a throwaway venv: `pip install --require-hashes -r requirements.lock.txt` succeeds and `pip check` is clean; `python -c "import acnportal.acnsim"` succeeds. |
| 11 | Add `.github/workflows/xval.yml`. | Push the branch, then `gh workflow run xval.yml --ref xval-fold` -> green. **Negative control:** temporarily set `_xval.TOLERANCE = 0.0` (or perturb one fixture), dispatch again, confirm red *and* that the diff table appears in the job summary and `xval_runs/` uploaded as an artifact. Restore. |
| 12 | Update the claim-bearing prose: root `README.md` (the "agree to max \|delta\| = 0.0" paragraph), `CLAUDE.md` ("Cross-validation status" + "Active work"), `docs/VALIDATION.md` section 2, `docs/ROADMAP.md`, `docs/ACNSIM_V2B_PLAN.md` (erratum on the X-matrix policy names), plugin `README.md` + `KNOWN_ISSUES.md`, new `RESULTS.md`. | `rg -n 'edf-v2b\|llf-v2b\|60c76bb' README.md CLAUDE.md docs/ xval/` returns only lines inside explicitly historical context (the erratum, `RESULTS.md`'s history). `rg -n 'no open parity gaps' xval/` returns nothing. |
| 13 | Confirm the Rust-only contributor path is untouched. | In an environment where `python3 -c "import numpy"` **fails**: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && cargo build --release && python3 tools/referee.py examples/one_day /tmp/out && python3 tools/check_xval_sync.py` all pass. This is the non-vacuous form: the import failure proves the heavy toolchain really is absent. |
| 14 | Open PR1 (import + hygiene + CI + re-sync as separate signed commits on one branch). | CI green, xval workflow green (it is path-filtered and `xval/**` changed, so it must run: confirm it actually appears on the PR rather than being skipped). |
| 15 | **Merge with a merge commit** (section 1.4). | After merge: `git log --oneline --graph -15` on `main` shows the merged plugin history, not one squashed blob. |
| 16 | Post-merge: check GitHub's rendered license and language bar. | `gh api repos/rishavsen1/openv2b --jq '.license.spdx_id'` still reports the dual-license detection it reported before the merge (record the before value in Step 0); the repo page does not show Python as the primary language. |
| 17 | Retire the source repo (not before Step 16 is green, and not before an agreed cooling-off period). | `~/acnportal-v2b` is moved to `~/archive/`, not deleted; the bundle from Step 1 is kept off-repo. |

Estimated effort: Steps 1-6 and 10-17 are half a day. Step 7-9 (the bridge fixes, the two new
fixtures, and the red-team pass) dominate: 1-2 days under Option B, 3-5 under Option A.

### Rollback and abort criteria

Abort before Step 14 (nothing public has changed; delete `xval-fold`, the plugin repo is untouched) if:

- **A1** Step 0 contradicts the predictions in a way that changes the diagnosis, e.g. X1 is red
  (the I/O contract moved and must be fixed first) or X2/`edf` is *green* (then the drift story is
  wrong and this plan is built on a false premise).
- **A2** Step 4's negative control shows `exclude` cannot keep `xval/` out of the crate. Do not
  proceed with a misdescribed package; solve the packaging question first.
- **A3** Step 9's red-team pass cannot find a mutation that a reduced experiment kills. Then Option B
  has produced a vacuous suite, and the choice is Option A for that experiment or dropping the
  experiment outright and saying so.
- **A4** The re-derived `policy-1` mirror cannot reach 1e-6 and the residual cannot be attributed to
  a named divergence. Then scope the mirror set down to `uncontrolled` only, accept the loss of
  discharge-path parity, and record it as an open gap rather than shipping an unexplained delta.
- **A5** Any evidence that a mirror was transcribed from `src/policy/*.rs`. Revert that mirror
  entirely; a transcribed mirror is worse than no mirror, because it makes a false independence claim.
- **A6** The xval job's wall time exceeds ~15 minutes or it is flaky more than once in the first
  month. Drop the `pull_request` trigger and keep schedule + tag only.

Rollback after Step 15 (public):

- Prefer **forward fixes**. Reverting a merge commit (`git revert -m 1 <sha>`) leaves the mainline
  poisoned against a future re-merge (git believes the content is already merged), so a later
  re-import needs `git revert` of the revert or a fresh import.
- The genuinely reversible pieces are: `xval.yml` (delete the file), the `ci.yml` step (delete the
  step), and `Cargo.toml`'s `exclude` (which should never be reverted). If the whole fold must be
  undone, prefer `git rm -r xval && git commit -s` plus restoring the standalone repo from the
  Step 1 bundle, and say so in the commit message rather than pretending it never happened.

---

## 7. Risks and mitigations

| # | Risk | Mitigation | Residual |
|---|---|---|---|
| R1 | **The bridge refuses queueing scenarios by design** (no `never_connected` in acnportal). openv2b's own `examples/one_day`, `one_month`, `one_month_lossy` therefore cannot be cross-validated as shipped. | State it in `xval/README.md` and `RESULTS.md`: cross-validation covers *purpose-built fixtures*, never the shipped examples. Every fixture is authored with >= as many chargers as concurrent sessions and a unit test asserts it. The refusal is loud (`NotImplementedError`), so a fixture that drifts into queueing turns CI red rather than silently narrowing the claim. | The parity claim never covers the contention-with-queueing path. This is a real coverage hole; it belongs in `KNOWN_ISSUES.md` (it already is, item 1) and in the README's scoping sentence. |
| R2 | **acnportal pins rot.** Upstream is unmaintained since 2023-11-21; `setuptools>=81` already breaks it; Python 3.10 reaches end-of-life in October 2026. | Hash-pinned `requirements.lock.txt`; weekly schedule so rot is discovered on a Monday and not on release day; auto-issue distinguishing an install failure from a parity failure. Documented degradation path (4.6): the claim becomes explicitly historical, anchored to a commit and a run URL, rather than being quietly left red. | Eventually unfixable without pinning a container image or vendoring wheels. Both are one-step escalations from the documented path. |
| R3 | **Rust-only contributors are forced into a Python toolchain.** | `xval/` is not a workspace member, has no `build.rs` hook, no cargo alias, and no cargo test depends on it. The per-PR check (`tools/check_xval_sync.py`) is stdlib-only, exactly like the referee that CI already runs. `CONTRIBUTING.md` gains one sentence: the documented dev loop is unchanged and requires no pip install. Step 13 verifies this in an environment where numpy genuinely cannot be imported. | Contributors touching `src/policy/**` will see the heavy workflow run on their PR and may need to interpret its failure. Mitigated by the triage order embedded in the auto-issue body and in `xval/README.md`. |
| R4 | **The published crate misrepresents its license** (F12). | `exclude = ["/xval"]` + Step 4's negative control + C2 in the per-PR checker. | If 0.1.0 is already on crates.io, that release is immutable; confirm before publishing (2.2). |
| R5 | **Independence is claimed but silently violated** by a future transcription. | Section 3's written invariants, C4/C5 mechanical checks, `DERIVATION.md` per policy, and A5 as an abort criterion. | C5 is a textual heuristic and can be evaded by someone determined. The real control is the written rule plus review; state that honestly rather than overselling the checker. |
| R6 | **Renewed drift**: a future port change silently invalidates the parity claim again. | C6 (name-set tripwire, every PR) catches rename/delete; the path-filtered `pull_request` trigger catches semantic changes to `src/policy/**` by actually running the suite; the tag trigger catches everything before a release. Option B shrinks the drift surface to two policies, one of which is openv2b-native and effectively frozen. | A semantic change to `policy-1` inside a PR that does not touch the filtered paths would slip past until the next Monday. Acceptable: weekly detection, and releases are gated. |
| R7 | **False-green CI**: the cross-validation tests `skip` when the binary is missing (by design), so a misconfigured job reports success while validating nothing. | `OPENV2B_BIN` set explicitly to the workspace build; the job asserts zero skips; the provenance step prints the binary's sha256 into the summary. | None material once the zero-skip assertion is in place; note that it must be added, it does not exist today. |
| R8 | **Stale-binary validation on developer machines**: the plugin's discovery order falls back to `~/openv2b/target/release/openv2b`, which for this owner is the real checkout and may be an older build than the working tree. | CI sets `OPENV2B_BIN` explicitly; every experiment prints the resolved binary path and sha256 alongside its diff table; `RESULTS.md` records the sha256 that produced each number. | A developer who ignores the printed provenance can still fool themselves. |
| R9 | **PR merge method silently squashes the imported history** (1.4). | Explicit requirement in the plan; Step 15's verification (`git log --graph`) fails loudly if it happened. | Recoverable only by re-importing, which is cheap at this size. |
| R10 | **Path-filtered workflow made a required check** blocks unrelated PRs forever. | Explicit "do not mark required" in 4.2, with the always-run gate-job pattern named as the future fix if it is ever wanted. | None if the instruction is followed. |
| R11 | **Dependency-graph noise**: Dependabot permanently flags the deliberate old pins. | 2.3: leave security updates off or add an ignore list with the reason. | Cosmetic. |
| R12 | **Reduced coverage under Option B is forgotten** and the README slowly re-inflates the claim. | `RESULTS.md` is the single place numbers live and it lists the mirrored policy set; `MIRRORED_POLICIES.txt` is machine-checked; the README links rather than restates. | Prose discipline; there is no mechanical check for over-claiming in English. |

---

## Appendix: adversarial review log

Three passes. Each entry: what the review found, and what changed.

### Pass 1 (would this break the public repo's CI, packaging, or license story?)

| # | Finding | Change |
|---|---|---|
| 1.1 | `cargo package` would ship BSD-3 files inside a crate declaring MIT OR Apache-2.0 (F12). The first draft mentioned this as a risk but had no verification. | Added `exclude = ["/xval"]`, Step 4, its **negative control**, the over-reach counter-check on `tools/referee.py`, and C2 in the per-PR checker. |
| 1.2 | First draft put the drift tripwire in a Rust `#[test]` reading `MIRRORED_POLICIES.txt` -- a file `exclude` removes from the packaged crate, so `cargo package --verify` or a vendored crate would break. | Moved to `tools/check_xval_sync.py`, a stdlib CI step, in the same family as `referee.py`. Also made it per-PR and cheap, which the Rust test would not have been. |
| 1.3 | First draft edited `LICENSE-MIT`/`LICENSE-APACHE` to add a scope note. Editing canonical license texts is itself a licensing defect and breaks automated license identification. | Root license files are now explicitly **not** to be edited; scope lives in README, CONTRIBUTING, and PROVENANCE. |
| 1.4 | `CONTRIBUTING.md` says all contributions are under MIT OR Apache-2.0, which becomes false for `xval/` and makes future DCO sign-offs certify the wrong license. | Added the CONTRIBUTING exception to Step 5 and to the licensing table. |
| 1.5 | A path-filtered workflow marked as a required status check hangs every PR that does not touch those paths. | Explicit "do not mark required" plus the gate-job pattern (4.2, R10). |
| 1.6 | Scheduled workflows get auto-disabled after 60 days of repository inactivity, so "the cron will catch it" is not durable on its own. | Added the `push: tags` release gate as the non-rotting trigger and documented the auto-disable. |
| 1.7 | `gh issue create` on a fork PR fails (read-only token), converting a parity failure into a permissions error. | Gated the auto-issue on `github.event_name == 'schedule'`. |
| 1.8 | The BSD-3 rationale "acnportal is BSD so this must be BSD" is legally wrong (acnportal is not vendored; nothing compels the license). Writing a false legal claim in the provenance document is worse than writing none. | Section 2 now states BSD-3 is a *choice* for attribution symmetry, not an obligation. |
| 1.9 | Linguist would flip the repo's language bar to Python; the first draft proposed `linguist-vendored`, which asserts the code is third-party -- untrue. | Switched to `linguist-detectable=false`. |
| 1.10 | GitHub's dependency graph will permanently flag the deliberate `numpy<2`/`setuptools<81` pins. | Added R11 and the 2.3 guidance. |

### Pass 2 (hidden work, false claims, vacuous verifications)

| # | Finding | Change |
|---|---|---|
| 2.1 | The task framed re-sync as "the algorithm mirrors are stale". Inspection found the **charger-assignment replay** is stale too (F6/F7): HEAD prefers a bidirectional port for every car and drops unassignable cars permanently. That is engine-level, so it would have survived a purely algorithmic re-sync and quietly corrupted results. | Added 5.2.1 as required work under either option. |
| 2.2 | **X4 cannot detect that staleness** (F8): its V2B vehicle also arrives first, so both rules agree. X4 was going to be re-run, come back green, and be reported as validating the assignment replay. A vacuous green is worse than a red. | Added X4b (charge-only car arrives first) with the requirement that the two rules produce visibly different numbers. |
| 2.3 | `max_soc_kwh` and `heuristic_threshold_kw` (F9) are unknown to the bridge -- currently inert because no fixture sets them, so a re-run would pass without covering them. | Added 5.2.3/5.2.4 and X1b, whose green condition includes the departure SoC landing on the ceiling. |
| 2.4 | The first draft's Option A (full edf/llf re-sync) ignored `tools/referee.py` (F15), which already gives per-PR algorithm-level differential coverage. Option A's marginal value is therefore much lower than its cost and its transcription risk. | Rewrote 5.1 around that argument and flipped the recommendation to Option B, with the referee argument stated explicitly. |
| 2.5 | Option B silently drops X2's sharp mutation test (the 22 kWh clamp-order divergence depended on `edf`'s non-canonical emission order). The first draft claimed Option B "loses nothing important". False. | Added the named coverage loss in 5.1, and made Step 9 require a *demonstrated* mutation for the reduced X2; A3 aborts if none exists. |
| 2.6 | "Preserving history" was overstated: filter-repo changes every SHA. | Stated the caveat; the original 8 hashes go into the import commit message and `DERIVATION.md`, and an off-repo bundle is created first (Step 1). |
| 2.7 | A GitHub PR merged with "Squash and merge" destroys the imported history -- the entire point of Option A -- with no warning. | Added 1.4 and Step 15's `git log --graph` verification. |
| 2.8 | A fresh import via `cp -r` would drag in `.venv/`, `__pycache__/`, `.pytest_cache/`, `*.egg-info/`, `xval_runs/`, all present in the working tree. | Fallback C uses `git archive` (tracked files only); Step 3 verifies with `find`. |
| 2.9 | The cross-validation tests **skip** when the binary is absent, so a misconfigured CI job is green while testing nothing. | Added the zero-skip assertion, explicit `OPENV2B_BIN`, and the binary-provenance step (R7). |
| 2.10 | The plugin's binary discovery falls back to `~/openv2b/target/release/openv2b`, which on this machine is the live checkout: a developer can validate a stale binary against a modified tree. | Added R8; CI sets `OPENV2B_BIN` explicitly and every run prints the resolved path + sha256. |
| 2.11 | Claims that become false were listed but not tied to a step. | Step 12 enumerates all seven files and gives a `rg` verification that leaves only explicitly historical mentions. |
| 2.12 | "Rust-only contributors are unaffected" was unverifiable as written; openv2b's CI *already* requires `python3`, so "no Python needed" would have been a false claim. | Step 13 states the honest version (stdlib python3 already required; pip/numpy/acnportal must not be) and verifies it in an environment where `import numpy` fails. |
| 2.13 | Non-vacuity for X1-X4 lived only in the plugin README's prose, so a fixture that stopped exercising its guard would still pass. | 5.2.5 promotes each argument to an assertion, and Step 9 adds a four-mutation red-team pass. |
| 2.14 | The plugin README's "nothing in this repository is imported by it [openv2b]" becomes self-referential nonsense once the plugin *is* in that repository. | Added to Step 12's rewrite list. |

### Pass 3 (residual sweep)

| # | Finding | Change |
|---|---|---|
| 3.1 | The plan asserted the X-suite's failure modes without running anything. Building the whole plan on unverified predictions is the same error the plan is meant to fix. | Section 0's prediction table is labelled a prediction, Step 0 measures it before any commit, and A1 aborts if reality differs. |
| 3.2 | C6 (name-set tripwire) cannot see a *semantic* change under an unchanged name -- which is half of what `a743277` did. Presenting it as "the fix" would over-claim. | Stated the limitation in 3.3 and covered the other half with the path-filtered PR trigger and the release gate (R6). |
| 3.3 | The independence claim is spec-level, not idea-level: both implementations descend from `docs/SPEC.md`, which since `a743277` encodes the reference's algorithms. A reviewer will find this hole; better to write it down. | Added the closing paragraph of 3.2. |
| 3.4 | Reverting a merge commit poisons a future re-merge; the first draft's rollback said "revert the merge" with no caveat. | Rewrote the post-merge rollback around forward fixes and `git rm -r xval`, with the revert-of-a-revert wart named. |
| 3.5 | `requirements.txt` uses `==` but not hashes, so a re-uploaded artifact would go undetected in a job whose entire purpose is reproducibility. | Added `requirements.lock.txt` with `--generate-hashes` and `pip install --require-hashes`; `requirements.txt` is retained as the rationale document. |
| 3.6 | No degradation path for the day acnportal becomes uninstallable; CI would simply sit red forever and the README claim would rot a second time. | Added the explicit historical-claim path in 4.6 and R2. |
| 3.7 | Root `README.md` says "Status: v0.2-alpha" while `CLAUDE.md` says v0.4-alpha. Pre-existing, unrelated to this fold. | Noted here, deliberately **not** added to the checklist: fixing it inside this change would blur the diff. Flag it to the owner as a separate one-line fix. |
| 3.8 | The recommendation to mirror `policy-1` was made before reading it. Verified afterwards: `src/policy/heuristics.rs:186-217` is ~25 lines over two passes plus the shared `get_rate` taper and two toleranced predicates, so the "cheapest discharge-exercising mirror" claim holds. But reading it surfaced a collision the recommendation had glossed over -- **both passes emit a setpoint for the same session**, and the engine resolves it last-wins, which a `{station_id: [kW]}` dict cannot represent and which makes the clamp-order position ambiguous. Left unstated, this would have been discovered as an unexplained sub-kW residual in Step 9 and quite possibly "fixed" by loosening the tolerance. | Added 5.2.7 as an explicit open question to settle by reading the engine, with a fixture that distinguishes the two candidate orders. Also noted that it partly restores the emission-order coverage that 5.1's loss 2 gives up. |
| 3.9 | Steps 8 and 9 both said "add `policy-1`" without saying where its semantics come from, which is exactly the moment the independence rule (3.1.3) is most likely to be broken. | 5.2.7 and Step 8 now require the derivation source and the collision ruling to be written into `DERIVATION.md` as the mirror is built, not reconstructed afterwards. |
