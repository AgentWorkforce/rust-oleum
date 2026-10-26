//! One-line "how to fix it properly" hints for gate violations.
//!
//! The gate's output is often read by coding agents, and a bare
//! `panic: src/x.rs::f = 2 exceeds target 0` invites the cheapest dodge
//! (`unwrap_or_default()` swallowing the error). Each hint names the right
//! fix, the wrong dodge, and a source rule where one exists. Hints print once
//! per metric, after the violations list.
//!
//! To add a hint for a new metric, add a row to [`HINTS`] keyed by the prefix
//! its violation lines use (`"<key>: ..."`).

/// Hint key for baseline bookkeeping: entries that are loose, no longer
/// needed, or match nothing. Every such violation ends with
/// [`TIGHTEN_SUFFIX`], whatever metric it belongs to.
const TIGHTEN: &str = "baseline";

/// Suffix shared by every stale-baseline violation line.
const TIGHTEN_SUFFIX: &str = "(or run --tighten)";

/// Hint key shared by every configured lint group: their violations read
/// `lints.<group>: ...`.
const LINT_GROUPS: &str = "lints";

/// Violation key (the text before the first `": "`) -> one-line fix hint.
/// Ordered as the gate reports its rows; hints print in first-seen order.
const HINTS: &[(&str, &str)] = &[
    (
        "cyclomatic",
        "extract named helper fns or replace branching with a `match` / lookup table; \
         don't merely move the code into a closure (closures count toward the enclosing fn)",
    ),
    (
        "cognitive",
        "flatten nesting with early returns / `let ... else` and extract named helper fns; \
         don't merely move the code into a closure (closures count toward the enclosing fn)",
    ),
    (
        "halstead",
        "name intermediate values and extract helpers so each fn does one thing; \
         don't golf the expression or shorten identifiers (the operator/operand mix is what counts)",
    ),
    (
        "file_loc",
        "split the file into cohesive modules, one concern each (BeautifulRust §9); \
         don't cram statements onto fewer lines or delete docs and tests to fit",
    ),
    (
        "unsafe",
        "use a safe std API or a well-reviewed crate; if `unsafe` is truly needed, wrap it in a \
         small module behind a safe API with a `// SAFETY:` comment (BeautifulRust §8); \
         don't merge blocks into one bigger block to lower the count",
    ),
    (
        "panic",
        "return an error with `?` or handle the `None`/`Err` case explicitly; `expect(\"why\")` \
         still counts, so keep one only as a reviewed baseline entry; don't hide the failure \
         with `unwrap_or_default()` (BeautifulRust §5, Rust API Guidelines C-GOOD-ERR)",
    ),
    (
        "allow",
        "fix the lint; `#[expect(lint, reason = \"...\")]` still counts, so keep one only as a \
         reviewed baseline entry on the narrowest item (BeautifulRust §2); don't hoist it to a \
         module- or crate-level `#![allow]`",
    ),
    (
        "file_allow",
        "fix the lints and delete the attribute; set project-wide lint levels once in \
         `Cargo.toml` `[lints]`; an item-level `#[expect(lint, reason = \"...\")]` still counts, \
         so keep one only as a reviewed baseline entry (BeautifulRust §2); \
         don't scatter `#[allow]` onto each fn instead",
    ),
    (
        "coverage",
        "add tests that exercise the uncovered behavior and assert on its results \
         (BeautifulRust §11); don't add assertion-free tests that only execute lines",
    ),
    (
        "crap",
        "CRAP = complexity² × (1 − coverage)³ + complexity: test the fn's behavior with real \
         assertions and split it if it stays complex; don't add assertion-free tests to buy coverage",
    ),
    (
        "mutants",
        "add a test assertion that fails for each listed mutation (check exact values and \
         boundaries, not just `is_ok()`); don't skip the fn with `#[mutants::skip]` or exclude it",
    ),
    (
        "dead code",
        "delete the unused item, or wire it up if it should be used; \
         don't silence it with a leading `_` or `#[allow(dead_code)]`",
    ),
    (
        "redundant code",
        "apply clippy's suggestion (drop the redundant clone / borrow / return); \
         don't silence it with `#[allow]`",
    ),
    (
        "bare_panic",
        "say why it can't fail: `expect(\"why\")`, or `assert!(cond, \"msg with values\")`; \
         better still return an error with `?`; never swap in `unwrap_or_default()` to hide it \
         (BeautifulRust §5)",
    ),
    (
        "bare_allow",
        "fix the lint; if it truly doesn't apply, write `#[expect(lint, reason = \"...\")]` so \
         the suppression explains itself and warns once it is stale; don't move it to a \
         module-level `#![allow]` (BeautifulRust §2)",
    ),
    (
        "file_bare_allow",
        "fix the lints; set project-wide levels once in `Cargo.toml` `[lints]`, and give any \
         item-level suppression `#[expect(lint, reason = \"...\")]`; don't scatter bare \
         `#[allow]`s onto each fn instead (BeautifulRust §2)",
    ),
    (
        "deny_warnings",
        "delete `#![deny(warnings)]`-style attributes: a new toolchain's warnings would break \
         the build; enforce warnings-as-errors in CI (`cargo clippy -- -D warnings`) instead, and \
         don't hide the attribute behind `cfg_attr` (BeautifulRust §2)",
    ),
    (
        "pub_wrapper",
        "take `&T`, `&mut T` or `T` and let callers choose ownership; keep `Rc`/`Arc`/`RefCell` \
         inside the type; don't just hide the wrapper behind a type alias \
         (BeautifulRust §3, Rust API Guidelines C-CALLER-CONTROL)",
    ),
    (
        "get_prefix",
        "name the getter after the field (`fn name(&self)`), without `get_`; don't dodge it \
         with `fetch_` or `read_` prefixes (Rust API Guidelines C-GETTER)",
    ),
    (
        "missing_error_impl",
        "implement `Display` and `std::error::Error` (e.g. `#[derive(thiserror::Error)]`) for \
         every public error type; don't fall back to `String` or `Box<dyn Error>` errors \
         (Rust API Guidelines C-GOOD-ERR)",
    ),
    (
        LINT_GROUPS,
        "fix what the lint reports (clippy's suggestion and the lint's docs say how); \
         don't silence it with `#[allow]` or drop the lint from its group",
    ),
    (
        TIGHTEN,
        "run `rust-oleum --tighten` and commit the updated rust-oleum.toml to lock the gain in; \
         never raise a baseline ceiling by hand",
    ),
];

/// A fix hint for one metric.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hint {
    pub metric: &'static str,
    pub text: &'static str,
}

/// The [`HINTS`] key a violation line belongs to.
fn hint_key(violation: &str) -> &str {
    if violation.ends_with(TIGHTEN_SUFFIX) {
        return TIGHTEN;
    }
    let key = violation.split_once(": ").map_or(violation, |(key, _)| key);
    if key.starts_with("lints.") {
        return LINT_GROUPS;
    }
    key
}

/// One hint per metric with violations, in first-seen order. Violations of
/// metrics without a hint are skipped.
pub fn for_violations(violations: &[String]) -> Vec<Hint> {
    let mut hints: Vec<Hint> = Vec::new();
    for v in violations {
        let key = hint_key(v);
        let Some(&(metric, text)) = HINTS.iter().find(|(m, _)| *m == key) else {
            continue;
        };
        if !hints.iter().any(|h| h.metric == metric) {
            hints.push(Hint { metric, text });
        }
    }
    hints
}

/// Terminal rendering: a `how to fix:` list, or nothing without hints.
pub fn render_text(hints: &[Hint]) -> String {
    render(hints, "how to fix:\n", |h| {
        format!("  - {}: {}\n", h.metric, h.text)
    })
}

/// Markdown rendering for `--github-summary`, or nothing without hints.
pub fn render_markdown(hints: &[Hint]) -> String {
    render(hints, "\n**How to fix:**\n", |h| {
        format!("- **{}**: {}\n", h.metric, h.text)
    })
}

fn render(hints: &[Hint], heading: &str, line: impl Fn(&Hint) -> String) -> String {
    if hints.is_empty() {
        return String::new();
    }
    let mut out = String::from(heading);
    for h in hints {
        out.push_str(&line(h));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn metrics(hints: &[Hint]) -> Vec<&str> {
        hints.iter().map(|h| h.metric).collect()
    }

    #[test]
    fn one_hint_per_metric_in_first_seen_order() {
        let hints = for_violations(&lines(&[
            "panic: src/a.rs::f = 2 exceeds target 0 (new violation)",
            "cyclomatic: src/a.rs::g = 30 exceeds target 22 (new violation)",
            "panic: src/a.rs::h = 1 exceeds target 0 (new violation)",
        ]));
        assert_eq!(metrics(&hints), ["panic", "cyclomatic"]);
        assert!(hints[0].text.contains("unwrap_or_default()"));
        assert!(hints[0].text.contains("C-GOOD-ERR"));
        assert!(hints[1].text.contains("closure"));
    }

    #[test]
    fn stale_baseline_entries_get_the_tighten_hint_not_the_metric_hint() {
        let hints = for_violations(&lines(&[
            "panic: baseline entry src/a.rs::f matches no function; remove it (or run --tighten)",
            "coverage: 99.00% is more than 1 point above the enforced floor 90.00%; \
             raise the floor to 98.5 (or run --tighten)",
        ]));
        assert_eq!(metrics(&hints), ["baseline"]);
        assert!(hints[0].text.contains("rust-oleum --tighten"));
    }

    #[test]
    fn multi_word_keys_and_unknown_metrics() {
        let hints = for_violations(&lines(&[
            "dead code: unused_variables@src/lib.rs::tidy = 1 exceeds target 0 (new violation)",
            "redundant code: clippy::redundant_clone@src/a.rs::f = 1 exceeds target 0",
            "someday: a metric nobody wrote a hint for",
            "no separator at all",
        ]));
        assert_eq!(metrics(&hints), ["dead code", "redundant code"]);
        let groups = for_violations(&lines(&[
            "lints.docs: missing_docs@src/lib.rs#L1 = 1 exceeds target 0 (new violation)",
            "lints.panics: clippy::unwrap_used@src/a.rs::f = 2 exceeds target 0",
        ]));
        assert_eq!(metrics(&groups), ["lints"]);
        assert!(for_violations(&[]).is_empty());
    }

    #[test]
    fn every_hint_is_one_line_naming_a_fix_and_a_dodge() {
        for (key, text) in HINTS {
            assert!(!text.contains('\n'), "{key}");
            assert!(text.contains("don't") || text.contains("never"), "{key}");
            // Keys are unique, so lookups never shadow a row.
            assert_eq!(HINTS.iter().filter(|(k, _)| k == key).count(), 1, "{key}");
        }
    }

    /// Every violation line `report::build` can emit maps to a hint, so a
    /// reworded message or a new metric without a hint fails here.
    #[test]
    fn every_gate_violation_has_a_hint() {
        use crate::coverage::CrapScore;
        use crate::external::{LintCounts, MutantCounts};
        use crate::rust_metrics::{FileMetrics, FunctionMetrics, RustMetrics};
        use std::collections::BTreeMap;

        let config: crate::config::Config = toml::from_str(
            "[targets]\ncyclomatic_max = 1\ncognitive_max = 1\nhalstead_difficulty_max = 1.0\n\
             file_loc_max = 1\ncrap_max = 1.0\n[sources]\nrust_roots = []\n\
             [lint_groups.panics]\nlints = [\"clippy::unwrap_used\"]\n\
             [baseline]\ncoverage_min_pct = 60.0\n[baseline.panic]\n\"gone.rs::f\" = 1\n",
        )
        .unwrap();
        let rust = RustMetrics {
            files: vec![FileMetrics {
                file: "a.rs".into(),
                loc: 2,
                allow_count: 1,
                bare_allow_count: 1,
                deny_warnings_count: 1,
            }],
            functions: vec![FunctionMetrics {
                file: "a.rs".into(),
                name: "f".into(),
                start_line: 1,
                end_line: 2,
                cyclomatic: 2,
                cognitive: 2,
                halstead_difficulty: 2.0,
                unsafe_count: 1,
                panic_count: 1,
                allow_count: 1,
                bare_panic_count: 1,
                bare_allow_count: 1,
                pub_wrapper_count: 1,
                get_prefix_count: 1,
            }],
            error_types: BTreeMap::from([("a.rs::E".to_string(), 1)]),
        };
        let crap = [CrapScore {
            file: "a.rs".into(),
            name: "f".into(),
            cyclomatic: 2,
            coverage_pct: 0.0,
            crap: 6.0,
        }];
        let one = BTreeMap::from([("l@a.rs::f".to_string(), 1)]);
        let lints = LintCounts {
            dead_code: 1,
            redundant: 1,
            dead_code_items: one.clone(),
            redundant_items: one.clone(),
            groups: BTreeMap::from([(
                "panics".to_string(),
                crate::external::GroupCounts {
                    total: 1,
                    items: one,
                },
            )]),
            ..LintCounts::default()
        };
        let mutants = MutantCounts {
            missed: 1,
            ..MutantCounts::default()
        };
        let report = crate::report::build(
            &config,
            &rust,
            Some(50.0),
            Some(&crap),
            Some(&lints),
            Some(&mutants),
        );
        let hinted: Vec<&str> = metrics(&report.hints);
        for v in &report.violations {
            assert!(hinted.contains(&hint_key(v)), "no hint for {v}");
        }
        // Every row in HINTS was exercised: none is dead.
        assert_eq!(hinted.len(), HINTS.len(), "{hinted:?}");
    }

    #[test]
    fn renders_text_and_markdown_lists() {
        let hints = for_violations(&lines(&["unsafe: src/a.rs::f = 1 exceeds target 0"]));
        let text = render_text(&hints);
        assert!(
            text.starts_with("how to fix:\n  - unsafe: use a safe std API"),
            "{text}"
        );
        assert!(text.ends_with("lower the count\n"), "{text}");
        let md = render_markdown(&hints);
        assert!(
            md.starts_with("\n**How to fix:**\n- **unsafe**: use a safe"),
            "{md}"
        );
        assert_eq!(md.lines().count(), 3, "{md}");
        assert_eq!(render_text(&[]), "");
        assert_eq!(render_markdown(&[]), "");
    }
}
