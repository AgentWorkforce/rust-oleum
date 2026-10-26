# Measurement and comparison protocol, version 1

## Commands

```sh
mkdir -p .rust-oleum
rust-oleum measure --config rust-oleum.toml --coverage \
  --output .rust-oleum/start.json
# Edit code; keep the policy and existing test declarations fixed.
rust-oleum measure --config rust-oleum.toml --coverage \
  --output .rust-oleum/candidate-1.json
rust-oleum compare --before .rust-oleum/start.json \
  --after .rust-oleum/candidate-1.json
```

Subcommand options go after `measure` or `compare`. `measure` supports
`--features a,b`, `--all-features`, and `--no-default-features`; the same selection
is used for Clippy, tests, and coverage. `--all-features` conflicts with an
explicit feature list. It uses Cargo's configured target and otherwise default
features, rather than claiming to test every possible feature/target combination.

`measure` writes a new JSON file atomically, prints a short JSON completion
record to stdout, and sends progress to stderr. Output must end in `.json` and
its parent directory must exist. It never overwrites an existing file. It exits
0 when evidence is complete, even if the measured code has quality debt. Failed
checks abort without publishing a new snapshot. Command-line usage errors are
also nonzero.

`compare` prints JSON to stdout, or writes it to a new file with `--output`.
Exit codes are 0 (`improved`), 1 (`unchanged` or `regressed`), and 2
(`incomparable`, including invalid/missing snapshots). Output/usage failures
may instead be explained on stderr. Neither command mutates source or policy;
Cargo does execute the target repository's build scripts and tests.

## Evidence collection

- Require a Cargo workspace root, a lockfile, nonempty existing source roots,
  and at least one production Rust file. Local path dependencies outside the
  workspace and symlinks in the inventoried tree are rejected in this version.
- Run locked Cargo metadata and resolve lint patterns against the installed
  `clippy-driver -W help` inventory. Unknown or empty patterns/groups fail.
- Use a fresh temporary target directory. Run Clippy across the workspace and
  all targets, with `--force-warn` for every tracked lint. This prevents an
  attribute or lint cap from turning tracked debt into a false zero. A configured
  `clippy.toml` can still define intentional policy such as unwraps in tests;
  its contents are frozen for comparisons.
- Require successful process exit, valid JSON, a successful final
  `build-finished` record, and fresh compiler artifacts for every workspace
  member. Reject compiler errors and unknown/removed-lint diagnostics. Only
  workspace diagnostics contribute to the snapshot.
- Run `cargo test --workspace --all-targets --locked` and workspace doctests.
  With `--coverage`, cargo-llvm-cov runs the all-target tests instead; doctests
  still run separately when the workspace has library targets. Binary-only
  workspaces record doctests as `not_applicable`. Empty, malformed, incomplete, or out-of-workspace LCOV
  inputs fail. Coverage is required if the policy baseline already includes
  coverage/CRAP debt.
- Fingerprint Rust source files, manifests, lockfiles, build scripts, lint and
  Cargo configuration. Record tool versions, evaluator binary hash, selected
  features, resolved lint names, and hashes of relevant Rust/Cargo/compiler
  environment variables. Check the inventory, policy, and context again before
  publishing; edits during a run invalidate the measurement.

The fresh build is deliberately more expensive than the legacy gate. It avoids
trusting cached diagnostic replay or externally supplied logs. Temporary build
outputs and raw reports are removed when measurement ends.

## Snapshot data

Snapshots have `schema_version: 1`, a `snapshot_sha256`, frozen `policy`, and:

| Field | Meaning |
|---|---|
| `context` | Evaluator, compiler, Cargo/Clippy versions, resolved lint set, feature selection, build configuration and environment hashes |
| `inputs` | Sorted map of inventoried input paths to SHA-256 hashes |
| `entities` | Sorted inventory of source files and qualified production functions |
| `test_contracts` | Hashes of existing test functions and ancestor attributes, normalized through Rust tokens |
| `compilation_gates` | Conditional-compilation attributes grouped by source item; changes to existing gates require review |
| `checks` | `passed`, `not_measured`, or `not_applicable` (doctests without library targets) for each check |
| `observations` | Sorted map containing all source measurements and tracked lint counts, including existing debt |
| `findings` | All tracked Clippy diagnostics with code, message, file, line and column; no example cap |
| `baseline_needs_tightening` | Whether the legacy baseline can be reduced from this measurement |

An observation has `metric`, `item`, `entity`, `file`, `line`, `value`, `target`,
`scale`, and `direction`. Counts use scale 1; Halstead/CRAP use tenths (10);
coverage percentages use hundredths (100). The agent coverage percentage counts
only instrumented production-function lines, so additional covered test lines
cannot dilute uncovered production code. A tracefile with no matching production
function lines is rejected. The legacy gate retains its original aggregate. Values are rounded to the nearest
fixed-point unit. No floating-point tolerances are used during comparison.
`value: null` means unavailable, for example a function with no instrumented
lines. No score or success is inferred from missing coverage.

Function IDs include module, self type and trait where applicable. Ambiguous
duplicate function identities fail measurement. Legacy baseline keys remain
unchanged. Lints outside analyzed functions are aggregated by lint and file,
so merely moving lines does not rename debt; precise positions remain in
`findings`. New observations with no previous debt cannot earn improvement
credit. Snapshots omit timestamps and temporary build paths; unchanged inputs,
tooling and check results produce identical serialized evidence.

## Comparison rule

Both snapshots must use the same schema, evaluator binary, toolchain,
measurement profile, resolved lint list, build controls, and complete policy
(including baseline). Required checks must have passed and existing conditional-compilation gates
must be unchanged. Source contents may
change, and new source entities and tests may be added. Removed inputs/entities
and changed or removed existing test declarations require review and return
`incomparable`. A change from known to unknown coverage, or the reverse, for
an existing observation also returns `incomparable`.

Comparison uses target-relative debt, independently for every observation:

```text
lower: max(0, value - target)
higher: max(0, target - value)
```

- Any compatibility problem: `incomparable`.
- Otherwise, any debt increase: `regressed`, even if other debts decreased.
- Otherwise, any debt decrease: `improved`.
- Otherwise: `unchanged`.

A disappeared lint counts as zero only while its owning entity still exists
and the measurement context is unchanged. Missing source metrics do not count
as fixes. A new source item is checked against its target; it cannot manufacture
progress by increasing the number of compliant functions. Unmeasured optional
metrics remain unmeasured, and reductions below a target receive no credit.

Results include `measurement`, `quality_change`, `baseline_action`,
`eligible_to_tighten`, `reasons`, `improvements`, and `regressions`. Changes carry
their stable ID, numeric before/after values, target, scale, and before/after
excess. For a newly introduced source item, `before` is its target (the initial
zero-debt allowance), not a historical measurement. Existing lint absence is
represented as zero. Reasons and changes use stable ordering.

An improvement is eligible to keep after behavioral review. If the baseline
also needs updating, `baseline_action` is `tighten_required`. Do not use the
ordinary gate's exit status as the hill-climbing reward: it intentionally fails
on a loose baseline after good work. Leave the baseline frozen until the search
ends; changing it starts a new comparison context.

## Limits and trust

Version 1 measures source metrics, Clippy, tests, and optional line coverage/CRAP.
Mutation testing is explicitly `not_measured`; existing `--mutants` support in
the ordinary gate remains available. Existing test declarations are protected,
but generated tests, macro expansion, helper behavior, arbitrary runtime inputs,
external services, and all feature combinations are not fully modeled. A passing
test command may run zero tests. Source analysis is syntactic. Coverage is line
coverage, not branch coverage. These are observable constraints, not a proof of
equivalent behavior or a hermetic build.

Use deterministic tests and a controlled environment. Freeze the evaluator and
keep snapshots in trusted storage for unattended use. The checksum catches
accidental edits, but an agent able to rewrite both the snapshot and checksum
can forge it. The tool is not a security boundary against an adversarial editor.

To tighten lint or coverage baselines after accepting changes, generate fresh
logs/coverage with the same feature selection and resolved lint flags, then use
the legacy `--tighten --clippy-log ... --coverage ...` workflow and rerun the gate.
The owned measurement's temporary reports are not retained for reuse.
