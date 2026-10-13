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
grandfathered offender under its target, delete its baseline line, and the
ratchet tightens — it never loosens.

## What it checks

| Metric | Default target | Measured from |
|---|---|---|
| Cyclomatic complexity | ≤ 22 per function | source |
| Cognitive complexity | ≤ 22 per function | source |
| Halstead difficulty | ≤ 80 per function | source |
| Lines of code | ≤ 500 per file | source |
| Test coverage | 100% | `--coverage` lcov file |
| CRAP score | ≤ 25 per function | `--coverage` lcov file |
| Dead code | 0 findings per item, grandfathered | `--clippy-log` JSON |
| Redundant code | 0 findings per item, grandfathered | `--clippy-log` JSON |
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
```

The gate fails on a violation that is not in the baseline, or that exceeds
its grandfathered ceiling. For dead and redundant code the ceiling is a
finding count per item, and `dead_code_max` / `redundant_code_max` in
`[targets]` are the per-item allowance (default 0). `rust-oleum --write-baseline` regenerates the
section after a refactor. The coverage floor works the same way: measured
coverage may never drop below the baseline floor while you climb toward the
target.

## License

Apache-2.0 © Agent Workforce Incorporated
