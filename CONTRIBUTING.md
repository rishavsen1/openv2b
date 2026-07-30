# Contributing to openv2b

Thanks for your interest in contributing!

## Ground rules

1. **Clean provenance.** Do not paste code from any other project unless its license is
   MIT/BSD/Apache-compatible **and** you preserve its attribution. Never contribute code you have
   copied from a proprietary codebase, decompiled, or reproduced from memory of proprietary source.
   Describing an algorithm in your own words and implementing it fresh is fine; transcribing code
   is not. See `docs/PROVENANCE.md`.
2. **Sign off your commits** (`git commit -s`). We use the
   [Developer Certificate of Origin](https://developercertificate.org/): your sign-off certifies you
   have the right to submit the work under this project's licenses.
3. **Dual license.** All contributions are accepted under `MIT OR Apache-2.0`.

## Development workflow

```bash
cargo build            # must compile warning-free
cargo test             # all tests must pass
cargo fmt --check      # rustfmt, default style
cargo clippy -- -D warnings
```

CI runs all four on every pull request.

## What makes a good change

- **Small and focused**: one behavioral change per PR.
- **Tested**: new behavior needs a test that fails without the change. Physics/billing changes need
  an invariant or hand-computed golden test, not just a smoke test.
- **Deterministic**: no wall-clock time, no unseeded randomness in the simulation path.
- **Lightweight**: new runtime dependencies require discussion in an issue first. The core must stay
  free of solver, GUI, and async dependencies (optimization backends go behind a cargo feature).

## Adding a policy

Implement the `Policy` trait (`src/policy/mod.rs`), register it in the policy factory, and add:
- a unit test of its ordering/priority logic,
- an integration run over `examples/one_day/` asserting the standard invariants.

## Reporting bugs

Open a GitHub issue with the scenario files (or a minimized version), the command line, the expected
behavior, and the observed behavior. Simulation bugs with a reproducing scenario get fixed fastest.
