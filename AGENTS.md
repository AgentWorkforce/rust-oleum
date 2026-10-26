# Agent instructions

## Using rust-oleum to improve a target repository

1. Use a fixed rust-oleum binary and Rust toolchain for an optimization run.
   Run from the target's Cargo workspace root. Commit or otherwise protect the
   starting source, `Cargo.lock`, policy, and tests. Generate a lockfile before
   measurement if the project does not have one.
2. Create `.rust-oleum/` for snapshots. Run
   `rust-oleum measure --output .rust-oleum/start.json`. Include `--coverage`
   when coverage/CRAP is an objective or the baseline contains that debt.
   Use the same feature flags on every measurement. A nonzero exit means no
   usable new snapshot was produced. Fix the measurement failure first.
3. Read `observations`. Each entry has a numeric `value`, `target`, `scale`,
   `direction`, `entity`, and source location. `null` means unmeasured, never
   zero. For lower-is-better metrics, debt is `max(0, value - target)`; for
   higher-is-better metrics it is `max(0, target - value)`. Divide by `scale`
   for display. Choose one item with positive debt. Use `findings` for all
   tracked Clippy messages and their file/line/column locations.
4. Make one focused change that preserves intended behavior. Keep the
   evaluator, policy, manifests, lint configuration, and existing tests fixed.
   Adding tests is supported. Do not disable checks, add suppressions, remove
   code, or relax thresholds to obtain a better score. Review the actual diff:
   these metrics and passing tests cannot prove behavioral equivalence.
5. Run `measure` again with a new output filename. Run `compare --before
   <last-accepted.json> --after <candidate.json>`. Read the JSON even when its
   exit status is nonzero:
   - `improved` (0): no measured debt increased and at least one decreased.
     Review behavior, then keep the change and use this snapshot as the next
     comparison reference. Preserve the original snapshot too.
   - `unchanged` (1): no target-relative progress. Do not count it as a win.
   - `regressed` (1): at least one debt increased. Rework or revert your edit.
   - `incomparable` (2): evidence, scope, existing tests, or policy changed.
     Resolve the stated reason; do not treat this as improvement. Legitimate
     renames, test rewrites, or dependency/toolchain updates need a reviewed
     new starting point.
6. Leave the baseline unchanged during the search. A successful improvement
   can report `baseline_action: "tighten_required"`; this is bookkeeping,
   not a quality regression. Once accepted work is complete, regenerate fresh
   external inputs and use the existing `--tighten` workflow, then run the
   ordinary gate. Start a new measurement run after changing the baseline.
   Source-only `--tighten` leaves lint and coverage baselines untouched; supply
   matching Clippy and coverage inputs when tightening those metrics.

Do not sum unrelated metrics into a single reward. Do not reward improvements
below a metric's target. A snapshot's SHA-256 detects accidental changes; it is
not an authentication signature. Never edit snapshots or their checksums to
obtain acceptance. Protect evaluator and snapshot files from the editing agent
when running an unattended harness.

Full protocol and limitations: [docs/agent-protocol.md](docs/agent-protocol.md).

## Developing this repository

- Preserve the existing gate, `init`, and `--tighten` compatibility. Snapshot
  function identities may be richer than legacy baseline keys.
- Keep snapshot ordering, fixed-point arithmetic, and comparison outcomes
  deterministic. Schema changes require an explicit version decision.
- Add regression tests for measurement failures and false improvements, not
  only happy-path reports. `tests/agent.rs` exercises actual Cargo/Clippy runs.
- Validate with `cargo fmt --all -- --check`,
  `cargo clippy --locked --all-targets -- -D warnings`, and
  `cargo test --locked`. Run the coverage-backed self-gate for behavior changes.
- Do not lower this repository's thresholds or coverage floor to accommodate
  an implementation. Keep the earlier numeric-policy, mutation-report, and
  error-unwrap fixes.
