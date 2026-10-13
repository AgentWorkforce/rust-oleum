//! Parsers for externally produced inputs: a `cargo clippy
//! --message-format=json` log (per-item dead code + redundant code
//! findings) and a `cargo mutants` `outcomes.json` (surviving mutants).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::config::function_key;
use crate::rust_metrics::FunctionMetrics;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct LintCounts {
    /// Total dead-code findings (after de-duplication across targets).
    pub dead_code: usize,
    /// Total redundant-code findings (after de-duplication across targets).
    pub redundant: usize,
    pub examples: Vec<String>,
    /// Item key (see [`lint_item_key`]) -> dead-code findings under it.
    pub dead_code_items: BTreeMap<String, usize>,
    /// Item key (see [`lint_item_key`]) -> redundant-code findings under it.
    pub redundant_items: BTreeMap<String, usize>,
}

/// Lints that indicate unreachable / unused ("dead") code.
const DEAD_CODE_LINTS: &[&str] = &[
    "dead_code",
    "unused_variables",
    "unused_imports",
    "unused_mut",
    "unreachable_code",
    "unreachable_patterns",
    "unused_must_use",
    "unused_assignments",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LintKind {
    Dead,
    Redundant,
}

/// Redundant code is any `clippy::redundant_*` / `clippy::needless_*` /
/// `clippy::duplicate*` lint, plus a couple of named equivalents.
fn lint_kind(code: &str) -> Option<LintKind> {
    if DEAD_CODE_LINTS.contains(&code) {
        return Some(LintKind::Dead);
    }
    let redundant = code.starts_with("clippy::redundant")
        || code.starts_with("clippy::needless")
        || code.starts_with("clippy::duplicate")
        || code == "clippy::useless_conversion"
        || code == "clippy::let_and_return";
    redundant.then_some(LintKind::Redundant)
}

/// One dead/redundant diagnostic, located by its primary span. Ordering and
/// equality cover every field, so a `BTreeSet` drops the copies cargo emits
/// once per compiled target (lib, bin, test).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    code: String,
    file: String,
    line: usize,
    column: usize,
    message: String,
}

/// Parse a `cargo clippy --message-format=json` log into per-item
/// dead-code and redundant-code counts. Each finding is attributed to the
/// production function (from `functions`) whose span contains it.
///
/// Errors when the log looks stale: no `compiler-message` records at all
/// and no workspace crate actually rebuilt, which is what a warm cargo cache
/// (or a broken redirect) produces — silently passing would hide findings.
pub fn parse_clippy_log(
    path: &Path,
    repo_root: &Path,
    functions: &[FunctionMetrics],
) -> Result<LintCounts> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading clippy log {}", path.display()))?;
    let findings = clippy_findings(&text, repo_root)
        .with_context(|| format!("parsing clippy log {}", path.display()))?;
    Ok(tally(&findings, functions))
}

fn clippy_findings(text: &str, repo_root: &Path) -> Result<BTreeSet<Finding>> {
    let mut findings = BTreeSet::new();
    let mut messages = 0usize;
    let mut rebuilt_local = false;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("reason").and_then(Value::as_str) {
            Some("compiler-message") => {
                messages += 1;
                findings.extend(finding(&v, repo_root));
            }
            Some("compiler-artifact") => rebuilt_local |= is_rebuilt_local(&v),
            _ => {}
        }
    }
    if messages == 0 && !rebuilt_local {
        bail!(
            "the clippy log contains no compiler-message records and no workspace crate \
             was rebuilt, so it looks empty — likely produced from a warm cargo cache that \
             skipped linting. Force a re-check (`cargo clean -p <crate>` or `touch src/lib.rs`) \
             before running `cargo clippy --message-format=json`"
        );
    }
    Ok(findings)
}

/// A `compiler-artifact` for a path (workspace) package that was compiled
/// in this run rather than replayed from cache.
fn is_rebuilt_local(v: &Value) -> bool {
    let local = v
        .get("package_id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.contains("path+file://"));
    local && v.get("fresh").and_then(Value::as_bool) == Some(false)
}

fn finding(v: &Value, repo_root: &Path) -> Option<Finding> {
    let code = v.pointer("/message/code/code").and_then(Value::as_str)?;
    lint_kind(code)?;
    let message = v
        .pointer("/message/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let span = v
        .pointer("/message/spans")
        .and_then(Value::as_array)
        .and_then(|spans| {
            spans
                .iter()
                .find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true))
        });
    let field = |name: &str| {
        span.and_then(|s| s.get(name))
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
    };
    let file = span
        .and_then(|s| s.get("file_name"))
        .and_then(Value::as_str)
        .map_or_else(|| "<unknown>".to_string(), |f| repo_relative(f, repo_root));
    Some(Finding {
        code: code.to_string(),
        file,
        line: field("line_start"),
        column: field("column_start"),
        message: message.to_string(),
    })
}

/// Normalize a diagnostic file name to the repo-relative, `/`-separated
/// form `rust_metrics` uses.
fn repo_relative(file_name: &str, repo_root: &Path) -> String {
    let file = file_name.replace('\\', "/");
    let root = repo_root.to_string_lossy().replace('\\', "/");
    let root = root.trim_end_matches('/');
    let rel = file
        .strip_prefix(root)
        .and_then(|r| r.strip_prefix('/'))
        .unwrap_or(&file);
    rel.strip_prefix("./").unwrap_or(rel).to_string()
}

fn tally(findings: &BTreeSet<Finding>, functions: &[FunctionMetrics]) -> LintCounts {
    let mut counts = LintCounts::default();
    for f in findings {
        let (total, items) = match lint_kind(&f.code) {
            Some(LintKind::Dead) => (&mut counts.dead_code, &mut counts.dead_code_items),
            Some(LintKind::Redundant) => (&mut counts.redundant, &mut counts.redundant_items),
            None => continue,
        };
        *total += 1;
        let key = lint_item_key(&f.code, &f.file, f.line, functions);
        *items.entry(key).or_insert(0) += 1;
        if counts.examples.len() < 20 {
            counts
                .examples
                .push(format!("{}: {}:{}: {}", f.code, f.file, f.line, f.message));
        }
    }
    counts
}

/// Baseline key for a lint finding: `<code>@<file>::<function>` for the
/// innermost production function whose span contains `line`, else
/// `<code>@<file>#L<line>` (module-level items, test code).
pub fn lint_item_key(code: &str, file: &str, line: usize, functions: &[FunctionMetrics]) -> String {
    functions
        .iter()
        .filter(|f| f.file == file && f.start_line <= line && line <= f.end_line)
        .min_by_key(|f| f.end_line - f.start_line)
        .map_or_else(
            || format!("{code}@{file}#L{line}"),
            |f| format!("{code}@{}", function_key(&f.file, &f.name)),
        )
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct MutantCounts {
    pub total: usize,
    pub caught: usize,
    pub missed: usize,
    pub timeout: usize,
    pub unviable: usize,
    pub missed_examples: Vec<String>,
}

/// Parse `mutants.out/outcomes.json` from cargo-mutants. The file carries
/// authoritative top-level counts (`total_mutants`, `missed`, `caught`,
/// `timeout`, `unviable`); the per-outcome list is only walked to name the
/// missed mutants.
pub fn parse_mutants_outcomes(path: &Path) -> Result<MutantCounts> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading mutants outcomes {}", path.display()))?;
    let v: Value = serde_json::from_str(&text).context("parsing mutants outcomes JSON")?;
    let count = |key: &str| v.get(key).and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut counts = MutantCounts {
        total: count("total_mutants"),
        caught: count("caught"),
        missed: count("missed"),
        timeout: count("timeout"),
        unviable: count("unviable"),
        missed_examples: Vec::new(),
    };
    let outcomes = v
        .get("outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for o in &outcomes {
        if o.get("summary").and_then(Value::as_str) != Some("MissedMutant") {
            continue;
        }
        if counts.missed_examples.len() < 20 {
            if let Some(name) = o.pointer("/scenario/Mutant/name").and_then(Value::as_str) {
                counts.missed_examples.push(name.to_string());
            }
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("quality-{}-{name}", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn function(file: &str, name: &str, start_line: usize, end_line: usize) -> FunctionMetrics {
        FunctionMetrics {
            file: file.into(),
            name: name.into(),
            start_line,
            end_line,
            cyclomatic: 1,
            cognitive: 0,
            halstead_difficulty: 1.0,
            unsafe_count: 0,
            panic_count: 0,
            allow_count: 0,
        }
    }

    /// One `compiler-message` line with a primary span (plus a secondary
    /// span that must be ignored).
    fn diag(code: &str, file: &str, line: usize, col: usize, message: &str) -> String {
        format!(
            concat!(
                r#"{{"reason":"compiler-message","target":{{"kind":["lib"]}},"#,
                r#""message":{{"code":{{"code":"{code}"}},"message":"{message}","spans":["#,
                r#"{{"file_name":"{file}","line_start":999,"column_start":1,"is_primary":false}},"#,
                r#"{{"file_name":"{file}","line_start":{line},"column_start":{col},"is_primary":true}}"#,
                r#"]}}}}"#,
                "\n"
            ),
            code = code,
            file = file,
            line = line,
            col = col,
            message = message
        )
    }

    fn parse(name: &str, log: &str, functions: &[FunctionMetrics]) -> Result<LintCounts> {
        let path = write_temp(name, log);
        parse_clippy_log(&path, Path::new("/repo"), functions)
    }

    #[test]
    fn keys_findings_per_function_and_dedups_targets() {
        let functions = [
            function("src/a.rs", "outer", 10, 40),
            function("src/a.rs", "S::inner", 20, 25),
            function("src/b.rs", "outer", 1, 100),
        ];
        let mut log = String::new();
        // The same warning from the lib and the test target: counted once.
        log += &diag(
            "unused_variables",
            "src/a.rs",
            12,
            9,
            "unused variable: `x`",
        );
        log += &diag(
            "unused_variables",
            "src/a.rs",
            12,
            9,
            "unused variable: `x`",
        );
        // A second, distinct finding of the same lint in the same function.
        log += &diag(
            "unused_variables",
            "src/a.rs",
            30,
            9,
            "unused variable: `y`",
        );
        // Innermost function wins; Windows separators and absolute paths
        // under the repo root normalize.
        log += &diag(
            "clippy::needless_return",
            r"src\\a.rs", // JSON-escaped backslash
            22,
            5,
            "needless return",
        );
        log += &diag(
            "clippy::redundant_clone",
            "/repo/src/a.rs",
            23,
            5,
            "redundant clone",
        );
        // Module-level item: no enclosing function -> line key.
        log += &diag("unused_imports", "./src/a.rs", 3, 5, "unused import");
        log += &diag("clippy::unrelated_lint", "src/a.rs", 12, 1, "ignored");
        log += r#"{"reason":"build-finished","success":true}"#;
        let counts = parse("clippy-items.json", &log, &functions).unwrap();

        assert_eq!(counts.dead_code, 3);
        assert_eq!(counts.redundant, 2);
        assert_eq!(
            counts.dead_code_items,
            BTreeMap::from([
                ("unused_imports@src/a.rs#L3".to_string(), 1),
                ("unused_variables@src/a.rs::outer".to_string(), 2),
            ])
        );
        assert_eq!(
            counts.redundant_items,
            BTreeMap::from([
                ("clippy::needless_return@src/a.rs::S::inner".to_string(), 1),
                ("clippy::redundant_clone@src/a.rs::S::inner".to_string(), 1),
            ])
        );
        assert_eq!(counts.examples.len(), 5);
    }

    #[test]
    fn spanless_finding_still_counts() {
        let log = r#"{"reason":"compiler-message","message":{"code":{"code":"dead_code"},"message":"m"}}"#;
        let counts = parse("clippy-spanless.json", log, &[]).unwrap();
        assert_eq!(counts.dead_code, 1);
        assert!(counts
            .dead_code_items
            .contains_key("dead_code@<unknown>#L0"));
    }

    #[test]
    fn empty_log_is_an_error() {
        // Nothing at all, or only cached (fresh) artifacts: a warm cache.
        let fresh = r#"{"reason":"compiler-artifact","package_id":"path+file:///repo#a@0.1.0","fresh":true}"#;
        let dep = r#"{"reason":"compiler-artifact","package_id":"registry+https://x#d@1.0.0","fresh":false}"#;
        for (name, log) in [("e1", ""), ("e2", fresh), ("e3", dep)] {
            let err = parse(name, log, &[]).unwrap_err();
            let chain = format!("{err:#}");
            assert!(chain.contains("looks empty"), "{chain}");
            assert!(chain.contains("cargo clean -p"), "{chain}");
        }
        // A rebuilt workspace crate with zero diagnostics is genuinely clean.
        let rebuilt = r#"{"reason":"compiler-artifact","package_id":"path+file:///repo#a@0.1.0","fresh":false}"#;
        let counts = parse("e4", rebuilt, &[]).unwrap();
        assert_eq!(counts.dead_code + counts.redundant, 0);
        // A compiler-message of any kind (e.g. the warning summary) proves
        // the log is live.
        let summary = r#"{"reason":"compiler-message","message":{"code":null,"message":"1 warning emitted"}}"#;
        assert!(parse("e5", summary, &[]).is_ok());
    }

    #[test]
    fn each_lint_family_counts_and_gets_an_example() {
        let dead = ["dead_code", "unused_variables", "unreachable_code"];
        let redundant = [
            "clippy::redundant_closure",
            "clippy::needless_return",
            "clippy::duplicate_underscore_argument",
            "clippy::useless_conversion",
            "clippy::let_and_return",
        ];
        let log: String = dead
            .iter()
            .chain(redundant.iter())
            .chain(["clippy::unrelated_lint"].iter())
            .map(|c| diag(c, "src/a.rs", 1, 1, "m"))
            .collect();
        let counts = parse("clippy-families.json", &log, &[]).unwrap();
        assert_eq!(counts.dead_code, dead.len());
        assert_eq!(counts.redundant, redundant.len());
        // One example per flagged finding; unrelated lints add none.
        assert_eq!(counts.examples.len(), dead.len() + redundant.len());
    }

    #[test]
    fn example_lists_cap_at_twenty() {
        let log: String = (1..=25)
            .map(|i| diag("dead_code", "src/a.rs", i, 1, "m"))
            .collect();
        let counts = parse("clippy-cap.json", &log, &[]).unwrap();
        assert_eq!(counts.dead_code, 25);
        assert_eq!(counts.dead_code_items.len(), 25);
        assert_eq!(counts.examples.len(), 20);

        let outcomes: Vec<String> = (0..25)
            .map(|i| {
                format!(r#"{{"summary":"MissedMutant","scenario":{{"Mutant":{{"name":"m{i}"}}}}}}"#)
            })
            .collect();
        let json = format!(
            r#"{{"total_mutants":25,"missed":25,"caught":0,"timeout":0,"unviable":0,"outcomes":[{}]}}"#,
            outcomes.join(",")
        );
        let path = write_temp("outcomes-cap.json", &json);
        let counts = parse_mutants_outcomes(&path).unwrap();
        assert_eq!(counts.missed, 25);
        assert_eq!(counts.missed_examples.len(), 20);
        assert_eq!(counts.missed_examples[0], "m0");
    }

    #[test]
    fn counts_missed_mutants() {
        let json = r#"{"total_mutants":2,"missed":1,"caught":1,"timeout":0,"unviable":0,
        "outcomes":[
            {"summary":"Success","scenario":"Baseline"},
            {"summary":"CaughtMutant","scenario":{"Mutant":{"name":"a"}}},
            {"summary":"MissedMutant","scenario":{"Mutant":{"name":"replace foo -> bar"}}}
        ]}"#;
        let path = write_temp("outcomes.json", json);
        let counts = parse_mutants_outcomes(&path).unwrap();
        assert_eq!(counts.total, 2);
        assert_eq!(counts.caught, 1);
        assert_eq!(counts.missed, 1);
        assert_eq!(counts.missed_examples, vec!["replace foo -> bar"]);
    }
}
