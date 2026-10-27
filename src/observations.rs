//! Project measurements in stable, numeric form. No formatted report parsing.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};

use crate::config::{function_key, Config};
use crate::coverage::Coverage;
use crate::external::LintCounts;
use crate::rust_metrics::RustMetrics;
use crate::snapshot::{Direction, Observation};

pub struct Measurements {
    pub entities: BTreeSet<String>,
    pub observations: BTreeMap<String, Observation>,
}

pub fn collect(
    config: &Config,
    rust: &RustMetrics,
    lints: &LintCounts,
    coverage: Option<&Coverage>,
    inputs: &BTreeMap<String, String>,
) -> Result<Measurements> {
    let mut out = Measurements {
        entities: BTreeSet::from(["@project".into()]),
        observations: BTreeMap::new(),
    };
    out.entities
        .extend(inputs.keys().filter(|p| p.ends_with(".rs")).cloned());
    let t = &config.targets;
    for file in &rust.files {
        out.entities.insert(file.file.clone());
        for (metric, value, target) in [
            ("file_loc", file.loc as u64, t.file_loc_max as u64),
            (
                "file_allow",
                u64::from(file.allow_count),
                u64::from(t.file_allow_max),
            ),
            (
                "file_bare_allow",
                u64::from(file.bare_allow_count),
                u64::from(t.file_bare_allow_max),
            ),
            (
                "deny_warnings",
                u64::from(file.deny_warnings_count),
                u64::from(t.deny_warnings_max),
            ),
        ] {
            insert(
                &mut out,
                metric,
                &file.file,
                &file.file,
                None,
                Some(value),
                (target, 1),
            )?;
        }
    }
    for f in &rust.functions {
        let item = function_key(&f.file, &f.name);
        ensure!(
            out.entities.insert(item.clone()),
            "ambiguous function identity {item}; split cfg variants or use distinct names"
        );
        for (metric, value, target) in [
            ("cyclomatic", f.cyclomatic, t.cyclomatic_max),
            ("cognitive", f.cognitive, t.cognitive_max),
            ("unsafe", f.unsafe_count, t.unsafe_max),
            ("panic", f.panic_count, t.panic_max),
            ("allow", f.allow_count, t.allow_max),
            ("bare_panic", f.bare_panic_count, t.bare_panic_max),
            ("bare_allow", f.bare_allow_count, t.bare_allow_max),
            ("pub_wrapper", f.pub_wrapper_count, t.pub_wrapper_max),
            ("get_prefix", f.get_prefix_count, t.get_prefix_max),
        ] {
            insert(
                &mut out,
                metric,
                &item,
                &f.file,
                Some(f.start_line),
                Some(u64::from(value)),
                (u64::from(target), 1),
            )?;
        }
        insert(
            &mut out,
            "halstead",
            &item,
            &f.file,
            Some(f.start_line),
            Some(scaled(f.halstead_difficulty, 10)?),
            (scaled(t.halstead_difficulty_max, 10)?, 10),
        )?;
    }
    // Error types are keyed `<defining file>::<Type>`.
    for (item, value) in &rust.error_types {
        let file = item
            .rsplit_once("::")
            .map_or(item.as_str(), |(file, _)| file);
        out.entities.insert(item.clone());
        insert(
            &mut out,
            "missing_error_impl",
            item,
            file,
            None,
            Some(u64::from(*value)),
            (u64::from(t.missing_error_impl_max), 1),
        )?;
    }
    lint_items(
        &mut out,
        "dead_code",
        &lints.dead_code_items,
        config.targets.dead_code_max,
    )?;
    lint_items(
        &mut out,
        "redundant_code",
        &lints.redundant_items,
        config.targets.redundant_code_max,
    )?;
    for (name, counts) in &lints.groups {
        lint_items(
            &mut out,
            &format!("lints.{name}"),
            &counts.items,
            config.lint_groups[name].max,
        )?;
    }
    coverage_items(&mut out, config, rust, coverage)?;
    Ok(out)
}

/// All fixed-point values use nearest rounding, with no platform-dependent epsilon.
pub fn scaled(value: f64, scale: u64) -> Result<u64> {
    let scaled = (value * scale as f64).round();
    ensure!(
        scaled.is_finite() && scaled >= 0.0 && scaled < u64::MAX as f64,
        "metric cannot be represented at scale {scale}: {value}"
    );
    Ok(scaled as u64)
}

fn insert(
    out: &mut Measurements,
    metric: &str,
    item: &str,
    file: &str,
    line: Option<usize>,
    value: Option<u64>,
    (target, scale): (u64, u64),
) -> Result<()> {
    let observation = Observation {
        metric: metric.into(),
        item: item.into(),
        entity: item.into(),
        file: file.into(),
        line,
        value,
        target,
        scale,
        direction: if metric == "coverage" {
            Direction::Higher
        } else {
            Direction::Lower
        },
    };
    let id = observation.key();
    ensure!(
        out.observations.insert(id.clone(), observation).is_none(),
        "duplicate measurement {id}"
    );
    Ok(())
}

fn lint_items(
    out: &mut Measurements,
    metric: &str,
    items: &BTreeMap<String, usize>,
    target: usize,
) -> Result<()> {
    for (key, count) in items {
        let (lint, owner) = key
            .split_once('@')
            .ok_or_else(|| anyhow::anyhow!("invalid lint item {key}"))?;
        // Outside functions, aggregate per file so adding a line does not rename debt.
        let entity = owner.split_once("#L").map_or(owner, |(file, _)| file);
        ensure!(
            out.entities.contains(entity),
            "lint finding outside measured scope: {key}"
        );
        let file = entity.split_once("::").map_or(entity, |(file, _)| file);
        let item = format!("{lint}@{entity}");
        let id = format!("{metric}@{item}");
        let observation = out.observations.entry(id).or_insert_with(|| Observation {
            metric: metric.into(),
            item,
            entity: entity.into(),
            file: file.into(),
            line: None,
            value: Some(0),
            target: target as u64,
            scale: 1,
            direction: Direction::Lower,
        });
        observation.value = Some(
            observation
                .value
                .unwrap_or(0)
                .checked_add(*count as u64)
                .ok_or_else(|| anyhow::anyhow!("lint count overflow"))?,
        );
    }
    Ok(())
}

fn coverage_items(
    out: &mut Measurements,
    config: &Config,
    rust: &RustMetrics,
    coverage: Option<&Coverage>,
) -> Result<()> {
    let production = coverage.map(|c| c.for_functions(&rust.functions));
    if let Some(c) = &production {
        ensure!(
            c.instrumented_line_count() > 0,
            "coverage has no instrumented production function lines"
        );
    }
    insert(
        out,
        "coverage",
        "@project",
        "",
        None,
        production
            .as_ref()
            .map(|c| scaled(c.total_line_coverage(), 100))
            .transpose()?,
        (scaled(config.targets.coverage_min_pct, 100)?, 100),
    )?;
    let scores: BTreeMap<_, _> = coverage
        .map(|c| c.crap_scores(&rust.functions))
        .unwrap_or_default()
        .into_iter()
        .map(|c| (function_key(&c.file, &c.name), c.crap))
        .collect();
    for f in &rust.functions {
        let item = function_key(&f.file, &f.name);
        insert(
            out,
            "crap",
            &item,
            &f.file,
            Some(f.start_line),
            scores.get(&item).map(|v| scaled(*v, 10)).transpose()?,
            (scaled(config.targets.crap_max, 10)?, 10),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_include_every_function_and_stable_module_lint_buckets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\ntrait A { fn f() {} }\ntrait B { fn f() {} }\nstruct S;\nimpl A for S { fn f() {} }\nimpl B for S { fn f() {} }\n").unwrap();
        let config = Config::parse("[targets]\n[sources]\nrust_roots=[\"src\"]").unwrap();
        let rust = crate::rust_metrics::collect_for_measure(dir.path(), &config.sources.rust_roots)
            .unwrap();
        let inputs = BTreeMap::from([("src/lib.rs".into(), "hash".into())]);
        let lints = LintCounts {
            dead_code_items: BTreeMap::from([
                ("dead_code@src/lib.rs#L3".into(), 2),
                ("dead_code@src/lib.rs#L4".into(), 1),
            ]),
            ..Default::default()
        };
        let out = collect(&config, &rust, &lints, None, &inputs).unwrap();
        for name in ["f", "A::f", "B::f", "<S as A>::f", "<S as B>::f"] {
            assert!(out.entities.contains(&format!("src/lib.rs::{name}")));
        }
        assert_eq!(
            out.observations["dead_code@dead_code@src/lib.rs"].value,
            Some(3)
        );
        assert_eq!(out.observations["coverage@@project"].value, None);
        let path = dir.path().join("coverage.info");
        std::fs::write(&path, "SF:src/lib.rs\nDA:1,1\nend_of_record\n").unwrap();
        let coverage = Coverage::from_lcov(&path, dir.path()).unwrap();
        let out = collect(&config, &rust, &lints, Some(&coverage), &inputs).unwrap();
        assert_eq!(out.observations["coverage@@project"].value, Some(10000));
        assert_eq!(out.observations["crap@src/lib.rs::f"].value, Some(10));
        assert_eq!(out.observations["crap@src/lib.rs::A::f"].value, None);
        let mut duplicate = rust;
        duplicate.functions.push(duplicate.functions[0].clone());
        assert!(collect(&config, &duplicate, &lints, None, &inputs).is_err());
    }

    #[test]
    fn invalid_lint_locations_and_unrepresentable_numbers_fail() {
        let mut out = Measurements {
            entities: BTreeSet::from(["src/lib.rs".into()]),
            observations: BTreeMap::new(),
        };
        assert!(lint_items(
            &mut out,
            "dead_code",
            &BTreeMap::from([("bad-key".into(), 1)]),
            0
        )
        .is_err());
        assert!(lint_items(
            &mut out,
            "dead_code",
            &BTreeMap::from([("dead_code@outside.rs#L1".into(), 1)]),
            0
        )
        .is_err());
        for value in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] {
            assert!(scaled(value, 10).is_err());
        }
        assert_eq!(scaled(12.34, 10).unwrap(), 123);
        assert_eq!(scaled(12.36, 10).unwrap(), 124);
        assert_eq!(scaled(0.0, 100).unwrap(), 0);
    }
}
