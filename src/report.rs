//! Gate evaluation and rendering: compares measured values against targets
//! and the grandfathered baseline, prints the benchmark table, and yields the
//! process exit status.

use std::collections::BTreeMap;

use crate::config::{function_key, Config};
use crate::coverage::CrapScore;
use crate::external::{LintCounts, MutantCounts};
use crate::rust_metrics::RustMetrics;

mod safety;
use safety::safety_rows;

#[derive(Debug, serde::Serialize)]
pub struct MetricRow {
    pub metric: String,
    pub value: String,
    pub target: String,
    /// Meets the aspirational target.
    pub meets_target: bool,
    /// Passes the enforced gate (target, or grandfathered baseline).
    pub passes_gate: bool,
    pub detail: String,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub rows: Vec<MetricRow>,
    pub violations: Vec<String>,
}

impl Report {
    pub fn gate_passed(&self) -> bool {
        self.violations.is_empty()
    }

    /// Row that failed the enforced gate: any violation line carries the
    /// metric's key prefix.
    fn push_row(&mut self, key: &str, mut row: MetricRow) {
        row.passes_gate = !self
            .violations
            .iter()
            .any(|v| v.starts_with(&format!("{key}:")));
        self.rows.push(row);
    }
}

/// A per-item metric value that can be ratcheted against a baseline ceiling.
pub trait Ratchet: PartialOrd + Copy + std::fmt::Display {
    /// Whether `self` sits measurably below `ceiling`, i.e. the ceiling is
    /// looser than it needs to be.
    fn below(self, ceiling: Self) -> bool;
}

impl Ratchet for u32 {
    fn below(self, ceiling: Self) -> bool {
        self < ceiling
    }
}

impl Ratchet for usize {
    fn below(self, ceiling: Self) -> bool {
        self < ceiling
    }
}

impl Ratchet for Rounded {
    /// Floats compare in displayed tenths, with one tenth of slack: the
    /// baseline writer stores `measured + 0.1`, which must not read as stale.
    fn below(self, ceiling: Self) -> bool {
        tenths(self.0) + 1 < tenths(ceiling.0)
    }
}

/// A float expressed in whole tenths (the one-decimal display precision).
pub fn tenths(x: f64) -> i64 {
    (x * 10.0).round() as i64
}

/// Worst offenders over a per-item limit, honoring baseline allowances.
/// Baseline entries that are looser than the measurement, no longer needed,
/// or that match no measured item are violations too: the baseline is a
/// ratchet. `noun` names the item kind ("function", "file").
/// Returns (worst value, count over target).
fn check_items<V: Ratchet>(
    items: impl Iterator<Item = (String, V)>,
    target: V,
    baseline: &BTreeMap<String, V>,
    (metric, noun): (&str, &str),
    violations: &mut Vec<String>,
) -> (Option<(String, V)>, usize) {
    let mut worst: Option<(String, V)> = None;
    let mut over_target = 0;
    // Baselined keys -> highest measured value under that key.
    let mut matched: BTreeMap<String, V> = BTreeMap::new();
    for (key, value) in items {
        if worst.as_ref().is_none_or(|(_, w)| value > *w) {
            worst = Some((key.clone(), value));
        }
        if value > target {
            over_target += 1;
            match baseline.get(&key) {
                Some(allowed) if value <= *allowed => {}
                Some(allowed) => violations.push(format!(
                    "{metric}: {key} = {value} exceeds its grandfathered ceiling {allowed}"
                )),
                None => violations.push(format!(
                    "{metric}: {key} = {value} exceeds target {target} (new violation)"
                )),
            }
        }
        if baseline.contains_key(&key) {
            insert_max(&mut matched, key, value);
        }
    }
    stale_entries(target, baseline, &matched, (metric, noun), violations);
    (worst, over_target)
}

/// Insert `value` under `key`, keeping the larger value when the key repeats.
pub fn insert_max<V: PartialOrd + Copy>(map: &mut BTreeMap<String, V>, key: String, value: V) {
    let slot = map.entry(key).or_insert(value);
    *slot = std::cmp::max_by(*slot, value, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Flag baseline entries that no longer pin a measured value exactly.
fn stale_entries<V: Ratchet>(
    target: V,
    baseline: &BTreeMap<String, V>,
    matched: &BTreeMap<String, V>,
    (metric, noun): (&str, &str),
    violations: &mut Vec<String>,
) {
    for (key, allowed) in baseline {
        let msg = match matched.get(key) {
            None => format!(
                "{metric}: baseline entry {key} matches no {noun}; remove it (or run --tighten)"
            ),
            Some(v) if *v <= target => format!(
                "{metric}: {key} = {v} now meets the target {target}; \
                 remove its baseline entry (or run --tighten)"
            ),
            Some(v) if v.below(*allowed) => format!(
                "{metric}: {key} = {v} is below its grandfathered ceiling {allowed}; \
                 tighten the baseline to {v} (or run --tighten)"
            ),
            Some(_) => continue,
        };
        violations.push(msg);
    }
}

pub fn build(
    config: &Config,
    rust: &RustMetrics,
    coverage_pct: Option<f64>,
    crap: Option<&[CrapScore]>,
    lints: Option<&LintCounts>,
    mutants: Option<&MutantCounts>,
) -> Report {
    let mut report = Report::default();
    source_rows(config, rust, &mut report);
    safety_rows(config, rust, &mut report);
    coverage_row(config, coverage_pct, &mut report);
    crap_row(config, crap, &mut report);
    mutants_row(config, mutants, &mut report);
    lint_rows(config, lints, &mut report);
    report
}

/// Add a per-item metric row (worst value + violation accounting).
fn item_metric_row<V: Ratchet>(
    report: &mut Report,
    (key, noun): (&str, &str),
    label: &str,
    items: impl Iterator<Item = (String, V)>,
    target: V,
    baseline: &BTreeMap<String, V>,
) {
    let row = item_row(
        &mut report.violations,
        (key, noun),
        label,
        items,
        target,
        baseline,
    );
    report.push_row(key, row);
}

/// Build a per-item row, recording any violations under the `key` prefix.
fn item_row<V: Ratchet>(
    violations: &mut Vec<String>,
    (key, noun): (&str, &str),
    label: &str,
    items: impl Iterator<Item = (String, V)>,
    target: V,
    baseline: &BTreeMap<String, V>,
) -> MetricRow {
    let (worst, over) = check_items(items, target, baseline, (key, noun), violations);
    let worst_s = worst.map(|(k, v)| (k, v.to_string()));
    let detail = match &worst_s {
        Some((k, v)) => format!("{over} over target; worst: {k} = {v}"),
        None => String::new(),
    };
    MetricRow {
        metric: label.into(),
        value: worst_s.map(|(_, v)| v).unwrap_or_default(),
        target: format!("<= {target}"),
        meets_target: over == 0,
        passes_gate: true,
        detail,
    }
}

fn source_rows(config: &Config, rust: &RustMetrics, report: &mut Report) {
    let t = &config.targets;
    let b = &config.baseline;
    item_metric_row(
        report,
        ("cyclomatic", "function"),
        "Cyclomatic complexity (per fn)",
        rust.functions
            .iter()
            .map(|f| (function_key(&f.file, &f.name), f.cyclomatic)),
        t.cyclomatic_max,
        &b.cyclomatic,
    );
    item_metric_row(
        report,
        ("cognitive", "function"),
        "Cognitive complexity (per fn)",
        rust.functions
            .iter()
            .map(|f| (function_key(&f.file, &f.name), f.cognitive)),
        t.cognitive_max,
        &b.cognitive,
    );
    item_metric_row(
        report,
        ("halstead", "function"),
        "Halstead difficulty (per fn)",
        rust.functions.iter().map(|f| {
            (
                function_key(&f.file, &f.name),
                Rounded(f.halstead_difficulty),
            )
        }),
        Rounded(t.halstead_difficulty_max),
        &b.halstead
            .iter()
            .map(|(k, v)| (k.clone(), Rounded(*v)))
            .collect(),
    );
    item_metric_row(
        report,
        ("file_loc", "file"),
        "Lines of code per file",
        rust.files.iter().map(|f| (f.file.clone(), f.loc)),
        t.file_loc_max,
        &b.file_loc,
    );
}

fn coverage_row(config: &Config, coverage_pct: Option<f64>, report: &mut Report) {
    let t = &config.targets;
    let Some(pct) = coverage_pct else {
        report.rows.push(not_measured(
            "Test coverage (line)",
            &format!("{}%", t.coverage_min_pct),
            "run with --coverage lcov.info",
        ));
        return;
    };
    let floor = config
        .baseline
        .coverage_min_pct
        .unwrap_or(t.coverage_min_pct);
    if pct < floor {
        report.violations.push(format!(
            "coverage: {pct:.2}% is below the enforced floor {floor:.2}%"
        ));
    }
    if let Some(msg) =
        stale_coverage_floor(pct, t.coverage_min_pct, config.baseline.coverage_min_pct)
    {
        report.violations.push(msg);
    }
    report.push_row(
        "coverage",
        MetricRow {
            metric: "Test coverage (line)".into(),
            value: format!("{pct:.2}%"),
            target: format!("{}%", t.coverage_min_pct),
            meets_target: pct >= t.coverage_min_pct,
            passes_gate: true,
            detail: format!("enforced floor: {floor:.2}%"),
        },
    );
}

/// How far measured coverage may sit above the grandfathered floor before
/// the floor counts as stale. The baseline writer leaves half a point of
/// headroom (floor = measured - 0.5), so a full point gives a freshly
/// written floor another half point of tolerance for run-to-run noise.
pub const COVERAGE_SLACK_PCT: f64 = 1.0;

/// A grandfathered coverage floor that has fallen behind the measurement.
fn stale_coverage_floor(pct: f64, target: f64, floor: Option<f64>) -> Option<String> {
    let floor = floor?;
    if pct >= target {
        return Some(format!(
            "coverage: {pct:.2}% now meets the target {target}%; \
             remove the baseline coverage_min_pct (or run --tighten)"
        ));
    }
    (pct - floor > COVERAGE_SLACK_PCT + 1e-9).then(|| {
        format!(
            "coverage: {pct:.2}% is more than 1 point above the enforced floor {floor:.2}%; \
             raise the floor to {:.1} (or run --tighten)",
            (pct - 0.5).max(0.0)
        )
    })
}

fn crap_row(config: &Config, crap: Option<&[CrapScore]>, report: &mut Report) {
    let t = &config.targets;
    let Some(scores) = crap else {
        report.rows.push(not_measured(
            "CRAP score (per fn)",
            &format!("<= {}", t.crap_max),
            "needs --coverage",
        ));
        return;
    };
    item_metric_row(
        report,
        ("crap", "function"),
        "CRAP score (per fn)",
        scores
            .iter()
            .map(|s| (function_key(&s.file, &s.name), Rounded(s.crap))),
        Rounded(t.crap_max),
        &config
            .baseline
            .crap
            .iter()
            .map(|(k, v)| (k.clone(), Rounded(*v)))
            .collect(),
    );
}

fn mutants_row(config: &Config, mutants: Option<&MutantCounts>, report: &mut Report) {
    let t = &config.targets;
    let Some(m) = mutants else {
        report.rows.push(not_measured(
            "Surviving mutants",
            &format!("<= {}", t.surviving_mutants_max),
            "run cargo-mutants and pass --mutants outcomes.json",
        ));
        return;
    };
    if m.missed > t.surviving_mutants_max {
        report.violations.push(format!(
            "mutants: {} surviving mutants (max {}): {}",
            m.missed,
            t.surviving_mutants_max,
            m.missed_examples.join("; ")
        ));
    }
    report.push_row(
        "mutants",
        MetricRow {
            metric: "Surviving mutants".into(),
            value: m.missed.to_string(),
            target: format!("<= {}", t.surviving_mutants_max),
            meets_target: m.missed <= t.surviving_mutants_max,
            passes_gate: true,
            detail: format!(
                "{} tested, {} caught, {} unviable",
                m.total, m.caught, m.unviable
            ),
        },
    );
}

fn lint_rows(config: &Config, lints: Option<&LintCounts>, report: &mut Report) {
    let t = &config.targets;
    let b = &config.baseline;
    let Some(l) = lints else {
        for (name, max) in [
            ("Dead code findings", t.dead_code_max),
            ("Redundant code findings", t.redundant_code_max),
        ] {
            report.rows.push(not_measured(
                name,
                &format!("<= {max}/item"),
                "needs --clippy-log",
            ));
        }
        return;
    };
    lint_row(
        report,
        "dead code",
        "Dead code findings",
        (&l.dead_code_items, l.dead_code),
        t.dead_code_max,
        &b.dead_code,
    );
    lint_row(
        report,
        "redundant code",
        "Redundant code findings",
        (&l.redundant_items, l.redundant),
        t.redundant_code_max,
        &b.redundant_code,
    );
}

/// Per-item lint row: items (`lint@file::fn` -> count) are gated like
/// complexity, while the row shows the total finding count.
fn lint_row(
    report: &mut Report,
    key: &str,
    label: &str,
    (items, total): (&BTreeMap<String, usize>, usize),
    max: usize,
    baseline: &BTreeMap<String, usize>,
) {
    let mut row = item_row(
        &mut report.violations,
        (key, "finding"),
        label,
        items.iter().map(|(k, n)| (k.clone(), *n)),
        max,
        baseline,
    );
    row.value = total.to_string();
    row.target = format!("<= {max}/item");
    row.detail = if items.is_empty() {
        "no findings".into()
    } else {
        format!("{} items; {}", items.len(), row.detail)
    };
    report.push_row(key, row);
}

fn not_measured(metric: &str, target: &str, detail: &str) -> MetricRow {
    MetricRow {
        metric: metric.into(),
        value: "not measured".into(),
        target: target.into(),
        meets_target: false,
        passes_gate: true,
        detail: detail.into(),
    }
}

/// f64 wrapper displayed with one decimal so baselines compare stably.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub struct Rounded(pub f64);

impl std::fmt::Display for Rounded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}", self.0)
    }
}

pub fn render_table(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<34} {:>14} {:>10}  {:^6} {:^6}  {}\n",
        "metric", "value", "target", "target", "gate", "detail"
    ));
    out.push_str(&"-".repeat(110));
    out.push('\n');
    for row in &report.rows {
        out.push_str(&format!(
            "{:<34} {:>14} {:>10}  {:^6} {:^6}  {}\n",
            row.metric,
            row.value,
            row.target,
            if row.meets_target { "PASS" } else { "MISS" },
            if row.passes_gate { "PASS" } else { "FAIL" },
            row.detail,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn baseline_allows_grandfathered_violation() {
        let mut baseline = BTreeMap::new();
        baseline.insert("a.rs::big".to_string(), 28u32);
        let mut violations = Vec::new();
        let items = vec![
            ("a.rs::big".to_string(), 28u32),
            ("a.rs::ok".to_string(), 3u32),
        ];
        let (worst, over) = check_items(
            items.into_iter(),
            22,
            &baseline,
            ("cyclomatic", "function"),
            &mut violations,
        );
        assert_eq!(over, 1);
        assert_eq!(worst.unwrap().1, 28);
        assert!(violations.is_empty());
    }

    #[test]
    fn new_violation_fails_gate() {
        let mut violations = Vec::new();
        let items = vec![("a.rs::new_big".to_string(), 25u32)];
        check_items(
            items.into_iter(),
            22,
            &BTreeMap::new(),
            ("cyclomatic", "function"),
            &mut violations,
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("new violation"));
    }

    #[test]
    fn regression_beyond_ceiling_fails_gate() {
        let mut baseline = BTreeMap::new();
        baseline.insert("a.rs::big".to_string(), 25u32);
        let mut violations = Vec::new();
        let items = vec![("a.rs::big".to_string(), 26u32)];
        check_items(
            items.into_iter(),
            22,
            &baseline,
            ("cyclomatic", "function"),
            &mut violations,
        );
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("grandfathered ceiling"));
    }

    use crate::config::{Baseline, Config, Sources, Targets};
    use crate::coverage::CrapScore;
    use crate::external::{LintCounts, MutantCounts};
    use crate::rust_metrics::{FileMetrics, FunctionMetrics, RustMetrics};

    fn test_config() -> Config {
        Config {
            targets: Targets {
                cyclomatic_max: 10,
                cognitive_max: 10,
                halstead_difficulty_max: 50.0,
                file_loc_max: 100,
                coverage_min_pct: 100.0,
                crap_max: 25.0,
                surviving_mutants_max: 0,
                dead_code_max: 0,
                redundant_code_max: 0,
                ..Targets::default()
            },
            baseline: Baseline {
                coverage_min_pct: Some(80.0),
                file_loc: BTreeMap::from([("big.rs".to_string(), 150)]),
                cyclomatic: BTreeMap::new(),
                cognitive: BTreeMap::new(),
                halstead: BTreeMap::new(),
                crap: BTreeMap::new(),
                ..Baseline::default()
            },
            sources: Sources { rust_roots: vec![] },
        }
    }

    fn fn_metric(name: &str, cyclomatic: u32) -> FunctionMetrics {
        FunctionMetrics {
            file: "a.rs".into(),
            name: name.into(),
            start_line: 1,
            end_line: 10,
            cyclomatic,
            cognitive: 1,
            halstead_difficulty: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn build_reports_every_metric_and_gates_correctly() {
        let config = test_config();
        let rust = RustMetrics {
            files: vec![
                FileMetrics {
                    file: "ok.rs".into(),
                    loc: 100,
                    allow_count: 0,
                    ..Default::default()
                },
                FileMetrics {
                    file: "big.rs".into(),
                    loc: 150,
                    allow_count: 0,
                    ..Default::default()
                },
            ],
            // Exactly at target: passes; one above: new violation.
            functions: vec![fn_metric("at_limit", 10), fn_metric("over", 11)],
            ..Default::default()
        };
        let crap = vec![CrapScore {
            file: "a.rs".into(),
            name: "over".into(),
            cyclomatic: 11,
            coverage_pct: 0.0,
            crap: 132.0,
        }];
        let lints = LintCounts {
            redundant: 1,
            redundant_items: BTreeMap::from([(
                "clippy::needless_return@a.rs::over".to_string(),
                1,
            )]),
            ..LintCounts::default()
        };
        let mutants = MutantCounts {
            total: 3,
            caught: 3,
            missed: 0,
            timeout: 0,
            unviable: 0,
            missed_examples: vec![],
        };
        let report = build(
            &config,
            &rust,
            Some(80.0),
            Some(&crap),
            Some(&lints),
            Some(&mutants),
        );

        assert_eq!(report.rows.len(), 20);
        let row = |m: &str| report.rows.iter().find(|r| r.metric.contains(m)).unwrap();

        // cyclomatic: "over" (11) is a new violation; "at_limit" (10) is not.
        assert!(!row("Cyclomatic").meets_target);
        assert!(!row("Cyclomatic").passes_gate);
        assert!(report
            .violations
            .iter()
            .any(|v| v.contains("a.rs::over = 11")));
        assert!(!report.violations.iter().any(|v| v.contains("at_limit")));

        // file_loc: big.rs is grandfathered exactly at its ceiling.
        assert!(!row("Lines of code").meets_target);
        assert!(row("Lines of code").passes_gate);

        // coverage: exactly at the enforced floor passes the gate.
        assert_eq!(row("coverage").value, "80.00%");
        assert!(!row("coverage").meets_target);
        assert!(row("coverage").passes_gate);

        // crap: 132.0 over target 25 without baseline fails the gate.
        assert!(!row("CRAP").passes_gate);

        // mutants: zero missed passes both.
        assert!(row("mutants").meets_target);
        assert!(row("mutants").passes_gate);

        // lints: dead at max passes, redundant above max fails.
        assert!(row("Dead code").meets_target);
        assert!(row("Dead code").passes_gate);
        assert!(!row("Redundant").meets_target);
        assert!(!row("Redundant").passes_gate);

        assert!(!report.gate_passed());
        let table = render_table(&report);
        assert!(table.contains("Cyclomatic complexity (per fn)"));
        assert!(table.contains("80.00%"));
        assert!(table.contains("FAIL"));
    }

    #[test]
    fn clean_inputs_pass_the_gate() {
        let mut config = test_config();
        config.baseline = Baseline::default();
        let rust = RustMetrics {
            files: vec![FileMetrics {
                file: "ok.rs".into(),
                loc: 10,
                allow_count: 0,
                ..Default::default()
            }],
            functions: vec![fn_metric("small", 1)],
            ..Default::default()
        };
        let report = build(&config, &rust, Some(100.0), None, None, None);
        assert!(report.gate_passed());
        assert!(report.violations.is_empty());
        let cov = report
            .rows
            .iter()
            .find(|r| r.metric.contains("coverage"))
            .unwrap();
        assert!(cov.meets_target);
        // Coverage below the floor is a violation.
        config.baseline.coverage_min_pct = Some(80.0);
        let report = build(&config, &rust, Some(79.9), None, None, None);
        assert!(!report.gate_passed());
        assert!(report.violations[0].contains("below the enforced floor"));
    }

    #[test]
    fn worst_keeps_first_of_equal_values() {
        let mut violations = Vec::new();
        let items = vec![
            ("a".to_string(), 1u32),
            ("b".to_string(), 5u32),
            ("c".to_string(), 5u32),
            ("d".to_string(), 3u32),
        ];
        let (worst, _) = check_items(
            items.into_iter(),
            22,
            &BTreeMap::new(),
            ("m", "function"),
            &mut violations,
        );
        assert_eq!(worst.unwrap(), ("b".to_string(), 5));
    }

    #[test]
    fn coverage_exactly_at_floor_passes() {
        let mut config = test_config();
        config.baseline.coverage_min_pct = Some(80.0);
        config.baseline.file_loc.clear();
        let rust = RustMetrics::default();
        let report = build(&config, &rust, Some(80.0), None, None, None);
        assert!(report.gate_passed());
        // Exactly at the 100% target meets the target too.
        let report = build(&config, &rust, Some(100.0), None, None, None);
        let cov = report
            .rows
            .iter()
            .find(|r| r.metric.contains("coverage"))
            .unwrap();
        assert!(cov.meets_target);
    }

    #[test]
    fn each_scalar_metric_violates_above_its_max() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        let rust = RustMetrics::default();
        let lints = LintCounts {
            dead_code: 1,
            dead_code_items: BTreeMap::from([("dead_code@a.rs::f".to_string(), 1)]),
            ..LintCounts::default()
        };
        let mutants = MutantCounts {
            total: 1,
            caught: 0,
            missed: 1,
            timeout: 0,
            unviable: 0,
            missed_examples: vec!["m".into()],
        };
        let report = build(&config, &rust, None, None, Some(&lints), Some(&mutants));
        assert!(report
            .violations
            .iter()
            .any(|v| v.starts_with("dead code:")));
        assert!(report.violations.iter().any(|v| v.starts_with("mutants:")));
        assert!(!report.violations.iter().any(|v| v.starts_with("redundant")));
        assert_eq!(report.violations.len(), 2);

        // All-zero inputs produce no violations at zero maxima.
        let clean_lints = LintCounts::default();
        let clean_mutants = MutantCounts::default();
        let report = build(
            &config,
            &rust,
            None,
            None,
            Some(&clean_lints),
            Some(&clean_mutants),
        );
        assert!(report.gate_passed());
    }

    #[test]
    fn safety_rows_gate_and_honor_baselines() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        config.baseline.panic.insert("a.rs::legacy".into(), 2);
        config.baseline.file_allow.insert("old.rs".into(), 1);
        let mut legacy = fn_metric("legacy", 1);
        legacy.panic_count = 2;
        let mut risky = fn_metric("risky", 1);
        risky.unsafe_count = 1;
        risky.allow_count = 1;
        let rust = RustMetrics {
            files: vec![FileMetrics {
                file: "old.rs".into(),
                loc: 10,
                allow_count: 1,
                ..Default::default()
            }],
            functions: vec![legacy, risky],
            ..Default::default()
        };
        let report = build(&config, &rust, None, None, None, None);
        let row = |m: &str| report.rows.iter().find(|r| r.metric == m).unwrap();
        assert!(!row("Panic calls (per fn)").meets_target);
        assert!(row("Panic calls (per fn)").passes_gate);
        assert!(row("Lint suppressions (per file)").passes_gate);
        assert!(!row("Unsafe blocks (per fn)").passes_gate);
        assert!(!row("Lint suppressions (per fn)").passes_gate);
        assert!(report
            .violations
            .iter()
            .any(|v| v == "unsafe: a.rs::risky = 1 exceeds target 0 (new violation)"));
        assert!(report.violations.iter().any(|v| v.starts_with("allow:")));
        assert_eq!(report.violations.len(), 2, "{:?}", report.violations);
    }

    #[test]
    fn every_safety_and_api_row_gates_its_own_field_and_baseline() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        let f = FunctionMetrics {
            unsafe_count: 1,
            panic_count: 2,
            bare_panic_count: 3,
            allow_count: 4,
            bare_allow_count: 5,
            pub_wrapper_count: 6,
            get_prefix_count: 7,
            ..fn_metric("f", 1)
        };
        let file = FileMetrics {
            file: "x.rs".into(),
            allow_count: 8,
            bare_allow_count: 9,
            deny_warnings_count: 10,
            ..Default::default()
        };
        let rust = RustMetrics {
            files: vec![file],
            functions: vec![f],
            error_types: BTreeMap::from([("e.rs::Bad".into(), 1), ("e.rs::Good".into(), 0)]),
        };
        let report = build(&config, &rust, None, None, None, None);
        let new = |key: &str, item: &str, n: u32| {
            format!("{key}: {item} = {n} exceeds target 0 (new violation)")
        };
        assert_eq!(
            report.violations,
            vec![
                new("unsafe", "a.rs::f", 1),
                new("panic", "a.rs::f", 2),
                new("bare_panic", "a.rs::f", 3),
                new("allow", "a.rs::f", 4),
                new("bare_allow", "a.rs::f", 5),
                new("pub_wrapper", "a.rs::f", 6),
                new("get_prefix", "a.rs::f", 7),
                new("file_allow", "x.rs", 8),
                new("file_bare_allow", "x.rs", 9),
                new("deny_warnings", "x.rs", 10),
                new("missing_error_impl", "e.rs::Bad", 1),
            ]
        );
        for label in [
            "Unexplained panics (per fn)",
            "Unjustified allows (per fn)",
            "Rc/Arc/Box/.. in pub signatures",
            "get_ getters (per fn)",
            "Unjustified allows (per file)",
            "Warning groups denied in source",
            "Pub error types without Error",
        ] {
            let row = report.rows.iter().find(|r| r.metric == label).unwrap();
            assert!(!row.meets_target && !row.passes_gate, "{label}");
        }

        // Grandfathered at exactly the measured values, every row passes.
        let b = &mut config.baseline;
        let one = |n: u32| BTreeMap::from([("a.rs::f".to_string(), n)]);
        let file = |n: u32| BTreeMap::from([("x.rs".to_string(), n)]);
        (b.unsafe_, b.panic, b.bare_panic, b.allow) = (one(1), one(2), one(3), one(4));
        (b.bare_allow, b.pub_wrapper, b.get_prefix) = (one(5), one(6), one(7));
        (b.file_allow, b.file_bare_allow, b.deny_warnings) = (file(8), file(9), file(10));
        b.missing_error_impl = BTreeMap::from([("e.rs::Bad".to_string(), 1)]);
        let report = build(&config, &rust, None, None, None, None);
        assert!(report.gate_passed(), "{:?}", report.violations);
    }

    #[test]
    fn lint_items_are_grandfathered_per_key() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        config.baseline.dead_code = BTreeMap::from([
            ("dead_code@a.rs::old".to_string(), 2),
            ("unused_imports@a.rs#L3".to_string(), 1),
        ]);
        let rust = RustMetrics::default();
        let lints = |items: &[(&str, usize)]| LintCounts {
            dead_code: items.iter().map(|(_, n)| n).sum(),
            dead_code_items: items.iter().map(|(k, n)| (k.to_string(), *n)).collect(),
            ..LintCounts::default()
        };
        let gate = |l: &LintCounts| build(&config, &rust, None, None, Some(l), None);

        // Grandfathered items at (or under) their ceilings pass; the row
        // misses the target but passes the gate and shows the total.
        let report = gate(&lints(&[
            ("dead_code@a.rs::old", 2),
            ("unused_imports@a.rs#L3", 1),
        ]));
        assert!(report.gate_passed(), "{:?}", report.violations);
        let row = report
            .rows
            .iter()
            .find(|r| r.metric == "Dead code findings")
            .unwrap();
        assert_eq!(row.value, "3");
        assert!(!row.meets_target);
        assert!(row.passes_gate);

        // Fixing one finding and adding another elsewhere still fails.
        let report = gate(&lints(&[
            ("dead_code@a.rs::old", 1),
            ("dead_code@a.rs::new", 1),
        ]));
        // The ratchet also flags the now-loose ceiling on `old` and the
        // unmatched `unused_imports` entry, so three violations in total.
        assert_eq!(report.violations.len(), 3, "{:?}", report.violations);
        assert!(report.violations[0].starts_with("dead code: dead_code@a.rs::new = 1"));
        assert!(report.violations[0].contains("new violation"));
        assert!(report
            .violations
            .iter()
            .any(|v| v.contains("dead_code@a.rs::old = 1 is below its grandfathered ceiling 2")));
        assert!(report
            .violations
            .iter()
            .any(|v| v.contains("baseline entry unused_imports@a.rs#L3 matches no finding")));

        // Exceeding a grandfathered count is a regression.
        let report = gate(&lints(&[("dead_code@a.rs::old", 3)]));
        assert!(report.violations[0].contains("grandfathered ceiling 2"));
        let row = report
            .rows
            .iter()
            .find(|r| r.metric == "Dead code findings")
            .unwrap();
        assert!(!row.passes_gate);
    }

    #[test]
    fn rounded_displays_one_decimal() {
        assert_eq!(Rounded(1.26).to_string(), "1.3");
        assert_eq!(Rounded(80.0).to_string(), "80.0");
    }

    #[test]
    fn baseline_at_exact_ceiling_is_allowed() {
        let mut baseline = BTreeMap::new();
        baseline.insert("a.rs::big".to_string(), 30u32);
        let mut violations = Vec::new();
        check_items(
            vec![("a.rs::big".to_string(), 30u32)].into_iter(),
            22,
            &baseline,
            ("cyclomatic", "function"),
            &mut violations,
        );
        assert!(violations.is_empty());
    }

    #[test]
    fn value_at_target_is_not_a_violation() {
        let mut violations = Vec::new();
        let (worst, over) = check_items(
            vec![("a.rs::f".to_string(), 22u32)].into_iter(),
            22,
            &BTreeMap::new(),
            ("cyclomatic", "function"),
            &mut violations,
        );
        assert!(violations.is_empty());
        assert_eq!(over, 0);
        assert_eq!(worst.unwrap().1, 22);
    }

    #[test]
    fn push_row_marks_gate_failures() {
        let mut report = Report::default();
        report
            .violations
            .push("coverage: 50% is below the enforced floor 60%".into());
        report.push_row(
            "coverage",
            MetricRow {
                metric: "Test coverage (line)".into(),
                value: "50%".into(),
                target: "100%".into(),
                meets_target: false,
                passes_gate: true,
                detail: String::new(),
            },
        );
        assert!(!report.rows[0].passes_gate);
    }

    fn check_cyclomatic(baseline: &[(&str, u32)], items: &[(&str, u32)]) -> Vec<String> {
        let baseline = baseline.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let mut violations = Vec::new();
        check_items(
            items.iter().map(|(k, v)| (k.to_string(), *v)),
            22,
            &baseline,
            ("cyclomatic", "function"),
            &mut violations,
        );
        violations
    }

    #[test]
    fn loose_ceiling_fails_the_gate() {
        let v = check_cyclomatic(&[("src/x.rs::f", 99)], &[("src/x.rs::f", 30)]);
        assert_eq!(
            v,
            vec![
                "cyclomatic: src/x.rs::f = 30 is below its grandfathered ceiling 99; \
                 tighten the baseline to 30 (or run --tighten)"
            ]
        );
    }

    #[test]
    fn entry_meeting_the_target_must_be_removed() {
        let v = check_cyclomatic(&[("src/x.rs::f", 99)], &[("src/x.rs::f", 4)]);
        assert_eq!(
            v,
            vec![
                "cyclomatic: src/x.rs::f = 4 now meets the target 22; \
                 remove its baseline entry (or run --tighten)"
            ]
        );
        // Exactly at the target counts as meeting it.
        let v = check_cyclomatic(&[("src/x.rs::f", 30)], &[("src/x.rs::f", 22)]);
        assert!(v[0].contains("now meets the target"));
    }

    #[test]
    fn unmatched_entry_fails_the_gate() {
        let v = check_cyclomatic(&[("src/x.rs::gone", 30)], &[("src/x.rs::f", 3)]);
        assert_eq!(
            v,
            vec![
                "cyclomatic: baseline entry src/x.rs::gone matches no function; \
                 remove it (or run --tighten)"
            ]
        );
    }

    #[test]
    fn duplicate_keys_compare_the_highest_value() {
        // Two items sharing a key: the ceiling pins the worse one.
        let items = [("src/x.rs::f", 30), ("src/x.rs::f", 3)];
        assert!(check_cyclomatic(&[("src/x.rs::f", 30)], &items).is_empty());
    }

    #[test]
    fn unmatched_file_entry_names_a_file() {
        let mut config = test_config();
        config.baseline.coverage_min_pct = None;
        let report = build(&config, &RustMetrics::default(), None, None, None, None);
        assert_eq!(
            report.violations,
            vec!["file_loc: baseline entry big.rs matches no file; remove it (or run --tighten)"]
        );
        let row = report
            .rows
            .iter()
            .find(|r| r.metric.contains("Lines"))
            .unwrap();
        assert!(!row.passes_gate);
    }

    #[test]
    fn unmeasured_metrics_skip_baseline_checks() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        config.baseline.crap = BTreeMap::from([("a.rs::gone".to_string(), 99.0)]);
        // No coverage input: neither the crap map nor the floor is checked.
        let report = build(&config, &RustMetrics::default(), None, None, None, None);
        assert!(report.gate_passed(), "{:?}", report.violations);
        // With coverage measured the crap entry is unmatched.
        let report = build(
            &config,
            &RustMetrics::default(),
            Some(80.0),
            Some(&[]),
            None,
            None,
        );
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].starts_with("crap: baseline entry a.rs::gone"));
        let row = report
            .rows
            .iter()
            .find(|r| r.metric.contains("CRAP"))
            .unwrap();
        assert!(!row.passes_gate);
    }

    #[test]
    fn float_ceilings_allow_one_tenth_of_slack() {
        let r = |m: f64, c: f64| Rounded(m).below(Rounded(c));
        // The baseline writer stores measured + 0.1: not stale.
        assert!(!r(85.04, 85.1));
        assert!(!r(85.06, 85.2));
        assert!(!r(85.0, 85.0));
        // Two tenths of headroom is loose.
        assert!(r(85.0, 85.2));
        assert!(r(4.0, 99.0));
        assert!(1u32.below(2) && !2usize.below(2));

        let mut config = test_config();
        config.baseline.file_loc.clear();
        config.baseline.halstead = BTreeMap::from([("a.rs::f".to_string(), 90.0)]);
        let mut f = fn_metric("f", 1);
        f.halstead_difficulty = 60.0;
        let rust = RustMetrics {
            files: vec![],
            functions: vec![f],
            ..Default::default()
        };
        let report = build(&config, &rust, None, None, None, None);
        assert_eq!(
            report.violations,
            vec![
                "halstead: a.rs::f = 60.0 is below its grandfathered ceiling 90.0; \
                 tighten the baseline to 60.0 (or run --tighten)"
            ]
        );
    }

    #[test]
    fn coverage_floor_must_track_the_measurement() {
        let mut config = test_config();
        config.baseline.file_loc.clear();
        config.baseline.coverage_min_pct = Some(80.0);
        let rust = RustMetrics::default();
        // Within a point: passes (a fresh floor sits half a point under).
        assert!(build(&config, &rust, Some(80.5), None, None, None).gate_passed());
        assert!(build(&config, &rust, Some(81.0), None, None, None).gate_passed());
        assert!(!build(&config, &rust, Some(81.01), None, None, None).gate_passed());
        // Well above: raise the floor.
        let report = build(&config, &rust, Some(90.0), None, None, None);
        assert_eq!(
            report.violations,
            vec![
                "coverage: 90.00% is more than 1 point above the enforced floor 80.00%; \
                 raise the floor to 89.5 (or run --tighten)"
            ]
        );
        let row = report
            .rows
            .iter()
            .find(|r| r.metric.contains("coverage"))
            .unwrap();
        assert!(!row.passes_gate);
        // At the target: the floor is unnecessary.
        let report = build(&config, &rust, Some(100.0), None, None, None);
        assert!(report.violations[0].contains("now meets the target 100%"));
        // No floor configured: nothing to be stale.
        config.baseline.coverage_min_pct = None;
        assert!(build(&config, &rust, Some(100.0), None, None, None).gate_passed());
    }

    #[test]
    fn coverage_slack_boundary_is_inclusive() {
        let edge = COVERAGE_SLACK_PCT + 1e-9;
        assert_eq!(stale_coverage_floor(edge, 100.0, Some(0.0)), None);
        assert!(stale_coverage_floor(edge + 1e-6, 100.0, Some(0.0)).is_some());
    }

    #[test]
    fn insert_max_keeps_the_larger_value_per_key() {
        let mut m = BTreeMap::new();
        for (k, v) in [("a", 2.0), ("a", 5.0), ("a", 3.0), ("b", 1.0)] {
            insert_max(&mut m, k.into(), v);
        }
        assert_eq!(m, BTreeMap::from([("a".into(), 5.0), ("b".into(), 1.0)]));
    }
}
