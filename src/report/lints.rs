//! Clippy-log rows of the gate report: dead code, redundant code, and each
//! configured lint group, ratcheted per item.

use std::collections::BTreeMap;

use super::{item_row, not_measured, Report};
use crate::config::Config;
use crate::external::LintCounts;

pub(super) fn lint_rows(config: &Config, lints: Option<&LintCounts>, report: &mut Report) {
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
        for (name, group) in &config.lint_groups {
            report.rows.push(not_measured(
                &group_label(name),
                &format!("<= {}/item", group.max),
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
    let empty = BTreeMap::new();
    for (name, group) in &config.lint_groups {
        let (items, total) = l
            .groups
            .get(name)
            .map_or((&empty, 0), |g| (&g.items, g.total));
        lint_row(
            report,
            &group_key(name),
            &group_label(name),
            (items, total),
            group.max,
            b.lint_groups.get(name).unwrap_or(&empty),
        );
    }
}

/// Violation prefix for a configured lint group (namespaced so a group
/// name can never collide with a built-in metric key).
fn group_key(name: &str) -> String {
    format!("lints.{name}")
}

fn group_label(name: &str) -> String {
    format!("Lint group: {name}")
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
