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
| Test coverage | 100% | `--coverage` lcov file |
| CRAP score | ≤ 25 per function | `--coverage` lcov file |
| Dead code | 0 findings | `--clippy-log` JSON |
| Redundant code | 0 findings | `--clippy-log` JSON |
| Surviving mutants | 0 | `--mutants` outcomes file |

Source metrics work with zero setup and skip test code (`tests/`,
`*_tests.rs`, `#[cfg(test)]`). The rest gate only when you pass the input:

```sh
cargo llvm-cov --workspace --lcov --output-path lcov.info
cargo clippy --workspace --all-targets --message-format=json > clippy-log.json
cargo mutants --in-diff pr.diff

rust-oleum --coverage lcov.info --clippy-log clippy-log.json --mutants mutants.out/outcomes.json
```

## CI (GitHub Actions)

```yaml
- run: cargo install rust-oleum --locked
- run: cargo llvm-cov --workspace --lcov --output-path lcov.info
- run: cargo clippy --workspace --all-targets --message-format=json > clippy-log.json
- run: rust-oleum --coverage lcov.info --clippy-log clippy-log.json --github-summary "$GITHUB_STEP_SUMMARY"
```

The `--github-summary` flag renders the metric table on the workflow run page.

## How the ratchet works

`rust-oleum.toml` holds aspirational `[targets]` and a `[baseline]` listing
each existing violation at its current ceiling:

```toml
[targets]
cyclomatic_max = 22

[baseline.cyclomatic]
"src/parser.rs::parse_event" = 45   # grandfathered — may only go down
```

The gate fails on a violation that is not in the baseline, or that exceeds
its grandfathered ceiling. The coverage floor works the same way: measured
coverage may never drop below the baseline floor while you climb toward the
target.

The baseline must also stay tight, so improvements are locked in instead
of leaving room to slide back. The gate fails when:

- a ceiling is looser than the measured value (`parse_event` dropped from
  45 to 30 but its entry still says 45). Float metrics (Halstead, CRAP)
  compare at one decimal, with a tenth of slack;
- a grandfathered item now meets its target, so the entry is unnecessary;
- an entry matches no measured file or function (it was deleted or renamed);
- measured coverage is more than half a point above the baseline floor, or
  meets the target.

Entries for metrics that weren't measured in a run (CRAP and coverage
without `--coverage`) are left alone.

`rust-oleum --tighten` fixes all of this in place: it rewrites the
`[baseline]` section of `rust-oleum.toml` (everything above it is kept
byte-for-byte), removing stale entries and lowering loose ceilings. It
never grandfathers a regression or a new violation, so those still fail.
`rust-oleum --write-baseline` prints a freshly regenerated section instead.

## License

Apache-2.0 © Agent Workforce Incorporated
