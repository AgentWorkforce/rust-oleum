<div align="center">

# Rust-Oleum

**A protective coating for your codebase.**

A code-quality ratchet for Rust codebases.
Existing debt is grandfathered. New debt fails the build. The baseline only shrinks.

</div>

## Install

```sh
cargo install rust-oleum
```

## Use

```sh
rust-oleum init   # scan the repo, write rust-oleum.toml: targets + a baseline of today's violations
rust-oleum        # the gate: exits non-zero on any NEW violation or regression
```

That's it. Commit `rust-oleum.toml` and put `rust-oleum` in CI. Refactor a
grandfathered offender and the gate makes you lock the gain in (run
`rust-oleum --tighten`): the ratchet tightens, and it never loosens.

## What it checks

| Metric | Default target | Measured from |
|---|---|---|
| Cyclomatic complexity | ≤ 22 per function | source |
| Cognitive complexity | ≤ 22 per function | source |
| Halstead difficulty | ≤ 80 per function | source |
| Lines of code | ≤ 500 per file | source |
| `unsafe` blocks (`unsafe fn` counts 1) | 0 per function | source |
| Panic calls (`.unwrap()`, `.expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`, `assert!`/`assert_eq!`/`assert_ne!`) | 0 per function | source |
| Unexplained panics (`.unwrap()`, `.expect("")`, message-less `panic!()`/`unreachable!()`/`assert!`/`assert_eq!`/`assert_ne!`, `todo!`, `unimplemented!`) | 0 per function | source |
| Lint suppressions (`#[allow]` / `#[expect]`, each lint counted) | 0 per function | source |
| Unjustified suppressions (anything but `#[expect(lint, reason = "...")]`) | 0 per function | source |
| Lint suppressions outside functions (`#![allow]`, on mods/impls/types) | 0 per file | source |
| Unjustified suppressions outside functions | 0 per file | source |
| Warning groups denied in source (`#![deny(warnings)]`, `#![deny(clippy::all)]`, ...) | 0 per file | source |
| `Rc` / `RefCell` / `Arc` / `Box` / `Cell` / `Mutex` in a `pub` fn's parameter or return types | 0 per function | source |
| `get_`-prefixed getters (`pub fn get_x(&self)`) | 0 per function | source |
| Public error types (returned as `E` in a `pub fn`'s `Result<_, E>`) without an `std::error::Error` impl | 0 per type | source |
| Test coverage | 100% | `--coverage` lcov file |
| CRAP score | ≤ 25 per function | `--coverage` lcov file |
| Dead code | 0 findings per item, grandfathered | `--clippy-log` JSON |
| Redundant code | 0 findings per item, grandfathered | `--clippy-log` JSON |
| Any clippy / rustc lint, in [lint groups](#lint-groups) you define | `max` findings per item (default 0), grandfathered | `--clippy-log` JSON |
| Surviving mutants | 0 | `--mutants` outcomes file |

Source metrics work with zero setup and skip test code (`tests/`,
`*_tests.rs`, `#[cfg(test)]`). The rest gate only when you pass the input:

```sh
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo clippy --workspace --all-targets --message-format=json > clippy-log.json
cargo mutants --in-diff pr.diff

rust-oleum --coverage lcov.info --clippy-log clippy-log.json --mutants mutants.out/outcomes.json
```

Dead code (`dead_code`, `unused_*`, `unreachable_*`) and redundant code
(`clippy::redundant_*`, `clippy::needless_*`, `clippy::duplicate*`, ...)
findings are tracked per item, not as a project total, so fixing one
warning doesn't pay for adding another. An item is
`"<lint>@<file>::<function>"` — the production function enclosing the
finding — or `"<lint>@<file>#L<line>"` for findings outside any function
(module-level imports, test code). Its value is the number of findings
under that key; identical diagnostics that cargo emits once per target
(lib, bin, test) count once. `rust-oleum init --clippy-log clippy-log.json`
grandfathers today's findings.

### Panics and suppressions: explained vs. bare

Both are counted twice, and the two counts gate independently:

- `panic_max` / `[baseline.panic]` count **every** panic call, exactly as
  before. `bare_panic_max` / `[baseline.bare_panic]` count the unexplained
  subset: a panic is explained when it says why it can't happen
  (`.expect("index checked above")`, `assert!(n > 0, "n = {n}")`,
  `panic!("...")`). A message that is not a string literal counts as
  explained; a blank literal does not. `todo!` and `unimplemented!` are
  always bare.
- `allow_max` / `file_allow_max` count every suppressed lint, as before.
  `bare_allow_max` / `file_bare_allow_max` count those not written as
  `#[expect(lint, reason = "...")]` — `#[expect]` warns once the lint stops
  firing, so a justified suppression can't silently outlive its reason.

With every target at its default of 0, a bare `.unwrap()` fails both rows
and `.expect("why")` fails only the total. To adopt the "explained panics are fine" policy, raise `panic_max`
and keep `bare_panic_max = 0`: converting `.unwrap()` to
`.expect("why")` then pays down the ratchet, while swallowing the error with
`.unwrap_or_default()` no longer looks like the only way to make progress.

### Public API checks

Where clippy is silent, rust-oleum checks a few
[Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) rules on
items declared `pub` (not `pub(crate)`; module reachability isn't
resolved, and trait-impl methods never count):

- wrapper types (`Rc`, `RefCell`, `Arc`, `Box`, `Cell`, `Mutex`) in a `pub`
  fn's parameter or return types, each occurrence counted
  (`self: Arc<Self>` receivers excepted);
- `pub fn get_x(&self)` getters (`get_mut`, `get_ref`, `get_unchecked`,
  `get_unchecked_mut` are fine);
- error types defined in the crate and returned as `E` from a `pub` fn
  (or a `pub type ... = Result<_, E>` alias) that lack an
  `impl Error for E` or `#[derive(Error)]` / `#[derive(Snafu)]`. This is a
  crate-wide scan per `rust_roots` entry, matching types by name; the item
  key is `"<defining file>::<Type>"` with a value of 1 per offending type.

### Upgrading

New metrics default to a target of 0, so code that already violates one
fails the gate after upgrading even though the config still loads.
Grandfather it by running
`rust-oleum --write-baseline` and replacing the `[baseline]` section with
its output (or `rust-oleum init --force`).

### Lint groups

Dead and redundant code are two built-in groups; every other lint in the
log is ignored unless you put it in a group of your own. Each group gets
its own report row and is ratcheted per item exactly like dead code:

```toml
[lint_groups.panics]
lints = ["clippy::unwrap_used", "clippy::panic_in_result_fn"]
max = 0                    # allowed findings per item (default 0)

[lint_groups.style]
lints = ["clippy::needless_*"]   # a trailing * matches by prefix
```

Lint names are the codes clippy reports: `clippy::<name>` for clippy
lints, the bare name for rustc lints (`missing_docs`). Each lint may belong
to one group only: patterns in two groups that could match the same lint
(e.g. `clippy::needless_*` and `clippy::needless_bool`) are a config error.
A configured group takes precedence over the built-in groups, so the
`style` group above claims `clippy::needless_*` findings that would
otherwise count as redundant code. Grandfathered items go under
`[baseline.lint_groups.<group>]` with the same keys as dead code, and
violations are reported as `lints.<group>: ...`.

A lint only reaches the log if it is enabled, and many useful ones are
allow-by-default: enable them in `Cargo.toml`'s `[lints]` table or with
`-W` flags when producing the log.

### Presets

`rust-oleum init --preset beautiful-rust` writes lint groups inspired by
the machine-checkable rules of
[BeautifulRust](https://github.com/morganlinton/BeautifulRust) (MIT),
pinned to its v1.2.0 lint list (commit `c9a062e`; see
[THIRD_PARTY_NOTICES](THIRD_PARTY_NOTICES)). The preset uses only its
individual lints: whole groups such as `clippy::pedantic` would flood the
baseline. It also adds `clippy::ptr_arg`, which upstream gets from
`clippy::all`. A test checks that the installed clippy recognises every
lint name in the preset.

| Group | Lints |
|---|---|
| `panics` | `clippy::unwrap_used`, `clippy::panic_in_result_fn` |
| `docs` | `missing_docs`, `clippy::missing_errors_doc`, `clippy::missing_panics_doc` |
| `api` | `clippy::fn_params_excessive_bools`, `clippy::struct_excessive_bools`, `clippy::ptr_arg`, `clippy::clone_on_ref_ptr`, `missing_debug_implementations`, `unreachable_pub` |
| `casts` | `clippy::as_conversions`, `clippy::cast_possible_truncation`, `clippy::cast_sign_loss` |
| `unsafe_hygiene` | `clippy::undocumented_unsafe_blocks`, `clippy::multiple_unsafe_ops_per_block`, `unsafe_op_in_unsafe_fn` |
| `suppressions` | `clippy::allow_attributes`, `clippy::allow_attributes_without_reason` |
| `leftovers` | `clippy::dbg_macro`, `clippy::todo`, `clippy::print_stdout`, `clippy::print_stderr` |

`init` prints the clippy command that enables them all:

```sh
cargo clippy --workspace --all-targets --message-format=json -- \
  -W clippy::unwrap_used -W clippy::panic_in_result_fn \
  -W missing_docs -W clippy::missing_errors_doc -W clippy::missing_panics_doc \
  -W clippy::fn_params_excessive_bools -W clippy::struct_excessive_bools -W clippy::ptr_arg \
  -W clippy::clone_on_ref_ptr -W missing_debug_implementations -W unreachable_pub \
  -W clippy::as_conversions -W clippy::cast_possible_truncation -W clippy::cast_sign_loss \
  -W clippy::undocumented_unsafe_blocks -W clippy::multiple_unsafe_ops_per_block -W unsafe_op_in_unsafe_fn \
  -W clippy::allow_attributes -W clippy::allow_attributes_without_reason \
  -W clippy::dbg_macro -W clippy::todo -W clippy::print_stdout -W clippy::print_stderr \
  > clippy-log.json
```

or enable them once in `Cargo.toml` (BeautifulRust's `lints/lints.toml`
is a ready-made version of this block):

```toml
[lints.rust]
missing_docs = "warn"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
unsafe_op_in_unsafe_fn = "warn"

[lints.clippy]
unwrap_used = "warn"
panic_in_result_fn = "warn"
# ... one line per clippy lint in the table above
```

and add a `clippy.toml` next to `Cargo.toml` so every `bool` parameter is
flagged and tests may unwrap and print:

```toml
max-fn-params-bools = 0
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-print-in-tests = true
```

## CI (GitHub Actions)

```yaml
- run: cargo install rust-oleum --locked
- run: cargo llvm-cov --workspace --lcov --output-path lcov.info
- run: cargo clippy --workspace --all-targets --message-format=json > clippy-log.json
- run: rust-oleum --coverage lcov.info --clippy-log clippy-log.json --github-summary "$GITHUB_STEP_SUMMARY"
```

The `--github-summary` flag renders the metric table on the workflow run page.

**Warm caches.** If cargo considers your crates up to date (e.g. a cached
`target/`, or a prior `cargo check`), `cargo clippy` may lint nothing and
the log would silently report zero findings. rust-oleum refuses such a log:
when it holds no `compiler-message` records and no workspace crate was
rebuilt, the run fails with an error. Force a re-lint before producing it:

```yaml
- run: touch src/lib.rs   # or: cargo clean -p <your-crate>
- run: cargo clippy --workspace --all-targets --message-format=json > clippy-log.json
```

## How the ratchet works

`rust-oleum.toml` holds aspirational `[targets]` and a `[baseline]` listing
each existing violation at its current ceiling:

```toml
[targets]
cyclomatic_max = 22

[baseline.cyclomatic]
"src/parser.rs::parse_event" = 45   # grandfathered — may only go down

[baseline.dead_code]
"unused_variables@src/parser.rs::parse_event" = 2   # two findings in this fn
"unused_imports@src/lib.rs#L4" = 1                  # module-level finding

[baseline.redundant_code]
"clippy::redundant_clone@src/cli.rs::run" = 1

[baseline.lint_groups.panics]
"clippy::unwrap_used@src/cli.rs::run" = 3
```

The gate fails on a violation that is not in the baseline, or that exceeds
its grandfathered ceiling. For dead and redundant code and lint groups the
ceiling is a finding count per item, and `dead_code_max` /
`redundant_code_max` in `[targets]` (a group's own `max`) are the per-item
allowance (default 0). The coverage floor works the same way: measured
coverage may never drop below the baseline floor while you climb toward the
target.

The baseline must also stay tight, so improvements are locked in instead
of leaving room to slide back. The gate fails when:

- a ceiling is looser than the measured value (`parse_event` dropped from
  45 to 30 but its entry still says 45). Float metrics (Halstead, CRAP)
  compare at one decimal, with a tenth of slack;
- a grandfathered item now meets its target, so the entry is unnecessary;
- an entry matches no measured file or function (it was deleted or renamed);
- measured coverage is more than a point above the baseline floor, or
  meets the target.

Entries for metrics that weren't measured in a run (CRAP and coverage
without `--coverage`, dead/redundant code and lint groups without `--clippy-log`) are
left alone.

`rust-oleum --tighten` fixes all of this in place: it rewrites the
`[baseline]` section of `rust-oleum.toml` (everything above it is kept
byte-for-byte), removing stale entries and lowering loose ceilings. It
never grandfathers a regression or a new violation, so those still fail.
`rust-oleum --write-baseline` prints a freshly regenerated section instead.

## License

Apache-2.0 © Agent Workforce Incorporated
