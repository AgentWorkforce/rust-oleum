//! Pure comparison: policy and measurement context are immutable during a search.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use serde::Serialize;

use crate::snapshot::{self, Observation, Snapshot};

#[derive(Debug, Args)]
pub struct CompareArgs {
    #[arg(long)]
    pub before: PathBuf,
    #[arg(long)]
    pub after: PathBuf,
    /// Write JSON to a new file instead of stdout.
    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Improved,
    Unchanged,
    Regressed,
    Incomparable,
}

#[derive(Debug, Serialize)]
pub struct Change {
    pub id: String,
    pub before: u64,
    pub after: u64,
    pub target: u64,
    pub scale: u64,
    pub excess_before: u64,
    pub excess_after: u64,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub schema_version: u32,
    pub measurement: &'static str,
    pub quality_change: Decision,
    pub baseline_action: &'static str,
    pub eligible_to_tighten: bool,
    pub reasons: Vec<String>,
    pub improvements: Vec<Change>,
    pub regressions: Vec<Change>,
}

pub fn run(args: CompareArgs) -> Result<i32> {
    let comparison = match (Snapshot::load(&args.before), Snapshot::load(&args.after)) {
        (Ok(before), Ok(after)) => compare(&before, &after),
        (Err(error), _) | (_, Err(error)) => Comparison {
            measurement: "incomparable",
            quality_change: Decision::Incomparable,
            ..empty_comparison(vec![format!("invalid snapshot: {error:#}")])
        },
    };
    if let Some(path) = args.output {
        snapshot::write_new(&path, &comparison)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&comparison)?);
    }
    Ok(match comparison.quality_change {
        Decision::Improved => 0,
        Decision::Unchanged | Decision::Regressed => 1,
        Decision::Incomparable => 2,
    })
}

pub fn compare(before: &Snapshot, after: &Snapshot) -> Comparison {
    let mut result = empty_comparison(compatibility(before, after));
    let keys: BTreeSet<_> = before
        .observations
        .keys()
        .chain(after.observations.keys())
        .collect();
    for key in keys {
        compare_item(
            key,
            before.observations.get(key),
            after.observations.get(key),
            &mut result,
        );
    }
    result.quality_change = if !result.reasons.is_empty() {
        result.measurement = "incomparable";
        Decision::Incomparable
    } else if !result.regressions.is_empty() {
        Decision::Regressed
    } else if !result.improvements.is_empty() {
        Decision::Improved
    } else {
        Decision::Unchanged
    };
    result.eligible_to_tighten = matches!(result.quality_change, Decision::Improved);
    if result.eligible_to_tighten && after.baseline_needs_tightening {
        result.baseline_action = "tighten_required";
    }
    result
}

fn empty_comparison(reasons: Vec<String>) -> Comparison {
    Comparison {
        schema_version: 1,
        measurement: "complete",
        quality_change: Decision::Unchanged,
        baseline_action: "none",
        eligible_to_tighten: false,
        reasons,
        improvements: Vec::new(),
        regressions: Vec::new(),
    }
}

fn compatibility(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let mut reasons = Vec::new();
    if before.context != after.context {
        reasons.push("measurement context changed (tools, enabled lints, features, environment or build configuration)".into());
    }
    if before.policy != after.policy {
        reasons.push(
            "policy changed, including targets, lint groups, source roots or baseline".into(),
        );
    }
    for (id, hash) in &before.test_contracts {
        if after.test_contracts.get(id) != Some(hash) {
            reasons.push(format!("existing test changed or disappeared: {id}"));
        }
    }
    for (id, hash) in &before.compilation_gates {
        let changed = after
            .compilation_gates
            .get(id)
            .map_or_else(|| *hash != snapshot::digest(b""), |current| current != hash);
        if changed {
            reasons.push(format!(
                "conditional compilation or item scope changed: {id}"
            ));
        }
    }
    for snapshot in [before, after] {
        required_checks(snapshot, &mut reasons);
    }
    for entity in before.entities.difference(&after.entities) {
        reasons.push(format!(
            "entity removed or renamed: {entity}; review the scope change"
        ));
    }
    for path in before
        .inputs
        .keys()
        .filter(|p| !after.inputs.contains_key(*p))
    {
        reasons.push(format!("input removed: {path}; review the scope change"));
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

fn required_checks(snapshot: &Snapshot, reasons: &mut Vec<String>) {
    for check in ["source", "clippy", "tests"] {
        if snapshot.checks.get(check).map(String::as_str) != Some("passed") {
            reasons.push(format!(
                "required check {check} has no successful measurement"
            ));
        }
    }
    let doctests = if snapshot.context.doctests {
        "passed"
    } else {
        "not_applicable"
    };
    if snapshot.checks.get("doctests").map(String::as_str) != Some(doctests) {
        reasons.push("doctest evidence does not match workspace targets".into());
    }
    if snapshot.context.coverage
        && snapshot.checks.get("coverage").map(String::as_str) != Some("passed")
    {
        reasons.push("required coverage has no successful measurement".into());
    }
}

fn compare_item(
    id: &str,
    before: Option<&Observation>,
    after: Option<&Observation>,
    result: &mut Comparison,
) {
    let template = match (before, after) {
        (Some(a), Some(b)) => {
            if a.metric != b.metric
                || a.target != b.target
                || a.scale != b.scale
                || a.direction != b.direction
                || a.entity != b.entity
            {
                result
                    .reasons
                    .push(format!("observation contract changed: {id}"));
                return;
            }
            a
        }
        (Some(a), None) if !a.is_lint() => {
            result
                .reasons
                .push(format!("measurement disappeared: {id}"));
            return;
        }
        (Some(a), _) | (_, Some(a)) => a,
        _ => return,
    };
    if before.is_some_and(|o| o.value.is_none()) || after.is_some_and(|o| o.value.is_none()) {
        if matches!((before, after), (Some(a), Some(b)) if a.value.is_some() != b.value.is_some()) {
            result
                .reasons
                .push(format!("measurement availability changed: {id}"));
        }
        return;
    }
    let old = before
        .and_then(|o| o.value)
        .unwrap_or(if template.is_lint() {
            0
        } else {
            template.target
        });
    let new = after.and_then(|o| o.value).unwrap_or(0);
    let mut old_observation = template.clone();
    old_observation.value = Some(old);
    let mut new_observation = template.clone();
    new_observation.value = Some(new);
    let old_excess = old_observation.excess().unwrap_or(0);
    let new_excess = new_observation.excess().unwrap_or(0);
    let change = Change {
        id: id.into(),
        before: old,
        after: new,
        target: template.target,
        scale: template.scale,
        excess_before: old_excess,
        excess_after: new_excess,
    };
    if new_excess < old_excess {
        result.improvements.push(change);
    } else if new_excess > old_excess {
        result.regressions.push(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{fixture, Direction};

    const KEY: &str = "cognitive@src/lib.rs::f";

    #[test]
    fn improvements_saturate_at_target_and_regressions_take_precedence() {
        let before = fixture();
        let mut after = before.clone();
        after.observations.get_mut(KEY).unwrap().value = Some(31);
        after.baseline_needs_tightening = true;
        let result = compare(&before, &after);
        assert_eq!(result.quality_change, Decision::Improved);
        assert_eq!(result.improvements[0].excess_before, 15);
        assert_eq!(result.improvements[0].excess_after, 9);
        assert_eq!(result.baseline_action, "tighten_required");
        assert!(result.eligible_to_tighten);
        let mut good = before.clone();
        good.observations.get_mut(KEY).unwrap().value = Some(20);
        let mut better = good.clone();
        better.observations.get_mut(KEY).unwrap().value = Some(10);
        assert_eq!(compare(&good, &better).quality_change, Decision::Unchanged);
        let mut extra = after.observations[KEY].clone();
        extra.metric = "panic".into();
        extra.target = 0;
        extra.value = Some(1);
        after.observations.insert(extra.key(), extra);
        let result = compare(&before, &after);
        assert_eq!(result.quality_change, Decision::Regressed);
        assert_eq!(result.improvements.len(), 1);
        assert_eq!(result.regressions.len(), 1);
        assert!(!result.eligible_to_tighten);
    }

    #[test]
    fn comparability_requires_same_policy_context_scope_and_required_checks() {
        let before = fixture();
        let mut cases = Vec::new();
        let mut s = before.clone();
        s.context.rustc = "new".into();
        cases.push(s);
        let mut s = before.clone();
        s.policy.targets.cognitive_max = 40;
        cases.push(s);
        let mut s = before.clone();
        s.checks.remove("tests");
        cases.push(s);
        let mut s = before.clone();
        s.checks.insert("clippy".into(), "failed".into());
        cases.push(s);
        let mut s = before.clone();
        s.checks.remove("doctests");
        cases.push(s);
        let mut s = before.clone();
        s.entities.clear();
        cases.push(s);
        let mut s = before.clone();
        s.inputs.clear();
        cases.push(s);
        let mut s = before.clone();
        s.observations.clear();
        cases.push(s);
        let mut s = before.clone();
        s.observations.get_mut(KEY).unwrap().scale = 10;
        cases.push(s);
        let mut s = before.clone();
        s.observations.get_mut(KEY).unwrap().value = None;
        cases.push(s);
        for s in cases {
            let result = compare(&before, &s);
            assert_eq!(result.quality_change, Decision::Incomparable);
            assert!(!result.reasons.is_empty());
            assert!(!result.eligible_to_tighten);
        }
        let mut coverage = before.clone();
        coverage.context.coverage = true;
        assert_eq!(
            compare(&coverage, &coverage).quality_change,
            Decision::Incomparable
        );
        coverage.checks.insert("coverage".into(), "passed".into());
        assert_eq!(
            compare(&coverage, &coverage).quality_change,
            Decision::Unchanged
        );
    }

    #[test]
    fn missing_lint_is_zero_but_missing_source_measurement_is_unknown() {
        let mut before = fixture();
        let mut lint = before.observations.remove(KEY).unwrap();
        lint.metric = "lints.docs".into();
        lint.target = 0;
        lint.value = Some(2);
        before.observations.insert(lint.key(), lint);
        let mut after = before.clone();
        after.observations.clear();
        assert_eq!(compare(&before, &after).quality_change, Decision::Improved);
        assert_eq!(compare(&after, &before).quality_change, Decision::Regressed);
        let mut unknown = fixture();
        unknown.observations.get_mut(KEY).unwrap().value = None;
        assert_eq!(
            compare(&unknown, &unknown).quality_change,
            Decision::Unchanged
        );
        let mut known = unknown.clone();
        known.observations.get_mut(KEY).unwrap().value = Some(5);
        assert_eq!(
            compare(&unknown, &known).quality_change,
            Decision::Incomparable
        );
    }

    #[test]
    fn coverage_improves_upward_and_line_movement_does_not_change_identity() {
        let mut before = fixture();
        let entry = before.observations.get_mut(KEY).unwrap();
        entry.direction = Direction::Higher;
        entry.target = 10000;
        entry.value = Some(8000);
        entry.scale = 100;
        let mut after = before.clone();
        let entry = after.observations.get_mut(KEY).unwrap();
        entry.value = Some(8500);
        entry.line = Some(100);
        assert_eq!(compare(&before, &after).quality_change, Decision::Improved);
        assert_eq!(compare(&after, &before).quality_change, Decision::Regressed);
    }

    #[test]
    fn additive_tests_are_allowed_but_existing_contracts_are_frozen() {
        let mut before = fixture();
        before
            .test_contracts
            .insert("test-a".into(), "hash-a".into());
        let mut after = before.clone();
        after
            .test_contracts
            .insert("test-b".into(), "hash-b".into());
        assert_eq!(compare(&before, &after).quality_change, Decision::Unchanged);
        after
            .test_contracts
            .insert("test-a".into(), "weakened".into());
        assert_eq!(
            compare(&before, &after).quality_change,
            Decision::Incomparable
        );
    }

    #[test]
    fn new_uninstrumented_helpers_are_allowed_but_cfg_changes_are_not() {
        let mut before = fixture();
        before
            .compilation_gates
            .insert("src/lib.rs::f".into(), snapshot::digest(b""));
        let mut after = before.clone();
        let mut helper = after.observations[KEY].clone();
        helper.metric = "crap".into();
        helper.item = "src/lib.rs::helper".into();
        helper.entity = helper.item.clone();
        helper.value = None;
        after.entities.insert(helper.entity.clone());
        after.observations.insert(helper.key(), helper);
        assert_eq!(compare(&before, &after).quality_change, Decision::Unchanged);
        after
            .compilation_gates
            .insert("src/lib.rs::f".into(), snapshot::digest(b"#[cfg(any())]"));
        assert_eq!(
            compare(&before, &after).quality_change,
            Decision::Incomparable
        );
        before
            .compilation_gates
            .insert("src/lib.rs::other-items".into(), snapshot::digest(b""));
        after = before.clone();
        after.compilation_gates.remove("src/lib.rs::other-items");
        assert_eq!(compare(&before, &after).quality_change, Decision::Unchanged);
    }
}
