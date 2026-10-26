//! Owned measurement: fresh Clippy, workspace tests and optional line coverage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use clap::Args;

use crate::config::Config;
use crate::coverage::Coverage;
use crate::evidence::{self, execute, strings};
use crate::snapshot::{self, ContextInfo, Snapshot};
use crate::{external, init, observations, rust_metrics, tighten};

#[derive(Debug, Args)]
pub struct MeasureArgs {
    /// New snapshot file; an existing file is never overwritten.
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long, default_value = "rust-oleum.toml")]
    pub config: PathBuf,
    /// Run cargo-llvm-cov and include coverage/CRAP evidence.
    #[arg(long)]
    pub coverage: bool,
    #[arg(long, value_delimiter = ',', conflicts_with = "all_features")]
    pub features: Vec<String>,
    #[arg(long)]
    pub all_features: bool,
    #[arg(long)]
    pub no_default_features: bool,
}

pub fn run(root: &Path, mut args: MeasureArgs) -> Result<()> {
    ensure!(
        !args.output.exists(),
        "output already exists: {}",
        args.output.display()
    );
    ensure!(
        args.output.extension().is_some_and(|s| s == "json"),
        "snapshot output must have a .json extension"
    );
    args.features.sort();
    args.features.dedup();
    let config = Config::load(&root.join(&args.config))?;
    evidence::validate_roots(root, &config)?;
    ensure!(
        args.coverage
            || (config.baseline.coverage_min_pct.is_none() && config.baseline.crap.is_empty()),
        "baseline has coverage/CRAP debt; measure requires --coverage"
    );
    let workspace = evidence::workspace(root)?;
    let inputs = evidence::inventory(root, &workspace.target)?;
    let test_contracts = crate::test_contracts::collect(root, &inputs)?;
    let rust = rust_metrics::collect_for_measure(root, &config.sources.rust_roots)?;
    ensure!(
        !rust.files.is_empty(),
        "no production Rust sources were measured"
    );
    let context = context(root, &args, &config, &inputs, workspace.has_library)?;
    let scratch = tempfile::tempdir().context("creating isolated measurement directory")?;
    let (lints, cov) = run_checks(
        root,
        &args,
        &config,
        &context.enabled_lints,
        &rust,
        &workspace,
        scratch.path(),
    )?;
    let measurements = observations::collect(&config, &rust, &lints, cov.as_ref(), &inputs)?;
    let needs_tightening = baseline_needs_tightening(root, &config, cov.as_ref(), scratch.path())?;
    ensure!(
        inputs == evidence::inventory(root, &workspace.target)?,
        "inputs changed during measurement; retry from a stable working tree"
    );
    ensure!(
        config == Config::load(&root.join(&args.config))?,
        "policy changed during measurement"
    );
    ensure!(
        context == self::context(root, &args, &config, &inputs, workspace.has_library)?,
        "measurement context changed during execution"
    );
    let mut snapshot = Snapshot {
        schema_version: snapshot::SCHEMA_VERSION,
        snapshot_sha256: String::new(),
        context,
        policy: config,
        inputs,
        test_contracts: test_contracts.tests,
        compilation_gates: test_contracts.gates,
        entities: measurements.entities,
        observations: measurements.observations,
        findings: lints.findings,
        checks: BTreeMap::from([
            ("source".into(), "passed".into()),
            ("clippy".into(), "passed".into()),
            ("tests".into(), "passed".into()),
            (
                "doctests".into(),
                if workspace.has_library {
                    "passed"
                } else {
                    "not_applicable"
                }
                .into(),
            ),
            (
                "coverage".into(),
                if args.coverage {
                    "passed"
                } else {
                    "not_measured"
                }
                .into(),
            ),
            ("mutants".into(), "not_measured".into()),
        ]),
        baseline_needs_tightening: needs_tightening,
    };
    snapshot.seal()?;
    snapshot.validate()?;
    snapshot::write_new(&args.output, &snapshot)?;
    println!(
        "{}",
        serde_json::json!({"measurement": "complete", "snapshot_sha256": snapshot.snapshot_sha256, "observations": snapshot.observations.len()})
    );
    Ok(())
}

fn context(
    root: &Path,
    args: &MeasureArgs,
    config: &Config,
    inputs: &BTreeMap<String, String>,
    doctests: bool,
) -> Result<ContextInfo> {
    let help = execute(root, "clippy-driver", &strings(&["-W", "help"]), None)?;
    Ok(ContextInfo {
        analyzer_version: env!("CARGO_PKG_VERSION").into(),
        analyzer_sha256: snapshot::digest(&std::fs::read(std::env::current_exe()?)?),
        rustc: execute(root, "rustc", &strings(&["-vV"]), None)?,
        cargo: execute(root, "cargo", &strings(&["--version"]), None)?,
        clippy: execute(root, "clippy-driver", &strings(&["--version"]), None)?,
        llvm_cov: args
            .coverage
            .then(|| execute(root, "cargo", &strings(&["llvm-cov", "--version"]), None))
            .transpose()?,
        enabled_lints: resolve_lints(&help, config)?,
        features: args.features.clone(),
        all_features: args.all_features,
        no_default_features: args.no_default_features,
        coverage: args.coverage,
        doctests,
        environment: evidence::environment(),
        controls: evidence::controls(root, inputs)?,
    })
}

fn resolve_lints(help: &str, config: &Config) -> Result<Vec<String>> {
    let known: BTreeSet<String> = help
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let level = fields.next()?;
            ["allow", "warn", "deny", "forbid"]
                .contains(&level)
                .then(|| name.replace('-', "_"))
        })
        .collect();
    ensure!(
        !known.is_empty(),
        "could not discover installed Clippy lints"
    );
    let mut enabled: BTreeSet<_> = known
        .iter()
        .filter(|lint| external::tracked_lint(lint, config))
        .cloned()
        .collect();
    for (name, group) in &config.lint_groups {
        ensure!(!group.lints.is_empty(), "lint group {name} has no lints");
        for pattern in &group.lints {
            ensure!(
                known
                    .iter()
                    .any(|lint| crate::config::pattern_matches(pattern, lint)),
                "lint pattern {pattern} has no matches in the installed toolchain"
            );
        }
    }
    // Unknown/renamed lint diagnostics must never be mistaken for clean code.
    enabled.extend(["unknown_lints".into(), "renamed_and_removed_lints".into()]);
    Ok(enabled.into_iter().collect())
}

fn cargo_args(command: &[&str], args: &MeasureArgs) -> Vec<String> {
    let mut flags = strings(command);
    flags.extend(strings(&["--workspace", "--locked"]));
    if args.all_features {
        flags.push("--all-features".into());
    }
    if args.no_default_features {
        flags.push("--no-default-features".into());
    }
    if !args.features.is_empty() {
        flags.extend(["--features".into(), args.features.join(",")]);
    }
    flags
}

fn run_checks(
    root: &Path,
    args: &MeasureArgs,
    config: &Config,
    lints: &[String],
    rust: &rust_metrics::RustMetrics,
    workspace: &evidence::Workspace,
    scratch: &Path,
) -> Result<(external::LintCounts, Option<Coverage>)> {
    let target = scratch.join("target");
    let mut flags = cargo_args(&["clippy", "--all-targets", "--message-format=json"], args);
    flags.push("--".into());
    for lint in lints {
        flags.extend(["--force-warn".into(), lint.clone()]);
    }
    eprintln!(
        "measure: running fresh workspace Clippy with {} enabled lints",
        lints.len()
    );
    let text = execute(root, "cargo", &flags, Some(&target))?;
    let log = evidence::validate_clippy(&text, &workspace.members)?;
    let path = scratch.join("clippy.json");
    std::fs::write(&path, log)?;
    let counts = external::parse_clippy_log(&path, root, &rust.functions, config)?;
    let cov = run_tests(root, args, &target, scratch, workspace.has_library)?;
    Ok((counts, cov))
}

fn run_tests(
    root: &Path,
    args: &MeasureArgs,
    target: &Path,
    scratch: &Path,
    doctests: bool,
) -> Result<Option<Coverage>> {
    eprintln!("measure: running workspace tests");
    let coverage = if args.coverage {
        let path = scratch.join("coverage.info");
        let mut flags = cargo_args(&["llvm-cov", "--all-targets", "--lcov"], args);
        flags.extend(["--output-path".into(), path.to_string_lossy().into_owned()]);
        execute(root, "cargo", &flags, Some(target))?;
        Coverage::validate_measurement(&path, root)?;
        Some(Coverage::from_lcov(&path, root)?)
    } else {
        execute(
            root,
            "cargo",
            &cargo_args(&["test", "--all-targets"], args),
            Some(target),
        )?;
        None
    };
    if doctests {
        eprintln!("measure: running workspace doctests");
        execute(
            root,
            "cargo",
            &cargo_args(&["test", "--doc"], args),
            Some(target),
        )?;
    }
    Ok(coverage)
}

fn baseline_needs_tightening(
    root: &Path,
    config: &Config,
    coverage: Option<&Coverage>,
    scratch: &Path,
) -> Result<bool> {
    // Baselines retain the original gate's identities for backwards compatibility.
    let rust = rust_metrics::collect(root, &config.sources.rust_roots)?;
    let lints =
        external::parse_clippy_log(&scratch.join("clippy.json"), root, &rust.functions, config)?;
    let crap = coverage.map(|c| c.crap_scores(&rust.functions));
    let fresh = init::baseline(
        config,
        &rust,
        coverage.map(Coverage::total_line_coverage),
        crap.as_deref(),
        Some(&lints),
    );
    let (_, summary) = tighten::clamp(
        &config.baseline,
        &fresh,
        tighten::Measured {
            coverage: coverage.is_some(),
            crap: coverage.is_some(),
            lints: true,
        },
    );
    Ok(summary.lowered > 0 || summary.removed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_resolution_is_sorted_expands_patterns_and_rejects_missing_checks() {
        let config = Config::parse("[targets]\n[sources]\nrust_roots=[\"src\"]\n[lint_groups.panics]\nlints=[\"clippy::unwrap_*\"]").unwrap();
        let help = "name default meaning\nclippy::unwrap-used allow unwrap\nclippy::unwrap-in-result warn unwrap\ndead-code warn dead\nclippy::all many-lints group\n";
        assert_eq!(
            resolve_lints(help, &config).unwrap(),
            [
                "clippy::unwrap_in_result",
                "clippy::unwrap_used",
                "dead_code",
                "renamed_and_removed_lints",
                "unknown_lints"
            ]
        );
        assert!(resolve_lints("", &config).is_err());
        assert!(resolve_lints("dead-code warn dead", &config).is_err());
        let empty = Config::parse(
            "[targets]\n[sources]\nrust_roots=[\"src\"]\n[lint_groups.empty]\nlints=[]",
        )
        .unwrap();
        assert!(resolve_lints(help, &empty).is_err());
    }

    #[test]
    fn selected_features_are_identical_for_every_cargo_phase() {
        let mut args = MeasureArgs {
            output: "snapshot.json".into(),
            config: "rust-oleum.toml".into(),
            coverage: false,
            features: vec!["a".into(), "b".into()],
            all_features: false,
            no_default_features: true,
        };
        assert_eq!(
            cargo_args(&["test", "--doc"], &args),
            strings(&[
                "test",
                "--doc",
                "--workspace",
                "--locked",
                "--no-default-features",
                "--features",
                "a,b"
            ])
        );
        args.features.clear();
        args.all_features = true;
        assert!(cargo_args(&["clippy"], &args).contains(&"--all-features".into()));
    }
}
