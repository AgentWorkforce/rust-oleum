//! `rust-oleum` — a code-quality ratchet gate.
//!
//! Measures a codebase against the targets in `rust-oleum.toml` (complexity,
//! Halstead difficulty, file size, coverage, CRAP, mutants, dead/redundant
//! code) and exits non-zero when the enforced gate fails.
//! Existing violations are grandfathered in a `[baseline]` section that may
//! only shrink: new violations and regressions fail, today's debt doesn't.

mod config;
mod coverage;
mod external;
mod halstead;
mod init;
mod loc;
mod report;
mod rust_metrics;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "rust-oleum",
    version,
    about = "A protective coating for your codebase: a code-quality ratchet gate"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to the config file.
    #[arg(long, default_value = init::CONFIG_FILE)]
    config: PathBuf,

    /// lcov tracefile from `cargo llvm-cov --lcov` (enables coverage + CRAP).
    #[arg(long)]
    coverage: Option<PathBuf>,

    /// JSON log from `cargo clippy --message-format=json` (enables dead /
    /// redundant code counts).
    #[arg(long)]
    clippy_log: Option<PathBuf>,

    /// `mutants.out/outcomes.json` from cargo-mutants (enables the surviving
    /// mutants count).
    #[arg(long)]
    mutants: Option<PathBuf>,

    /// Write the full report as JSON to this path.
    #[arg(long)]
    json: Option<PathBuf>,

    /// Print a regenerated `[baseline]` TOML section for every current
    /// violation of the targets, then exit 0 without gating.
    #[arg(long)]
    write_baseline: bool,

    /// Append a markdown summary to this path (e.g. $GITHUB_STEP_SUMMARY).
    #[arg(long)]
    github_summary: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Detect source roots, measure the project, and write a starter
    /// config whose [baseline] grandfathers today's violations.
    Init {
        /// lcov tracefile so the baseline includes a coverage floor and
        /// CRAP ceilings.
        #[arg(long)]
        coverage: Option<PathBuf>,
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let repo_root = std::env::current_dir().context("resolving working directory")?;

    if let Some(Command::Init { coverage, force }) = args.command {
        return init::run(&repo_root, coverage.as_deref(), force);
    }

    let config = config::Config::load(&repo_root.join(&args.config))?;
    let rust = rust_metrics::collect(&repo_root, &config.sources.rust_roots)?;

    let cov = args
        .coverage
        .as_deref()
        .map(|p| coverage::Coverage::from_lcov(p, &repo_root))
        .transpose()?;
    let coverage_pct = cov.as_ref().map(coverage::Coverage::total_line_coverage);
    let crap = cov.as_ref().map(|c| c.crap_scores(&rust.functions));

    let lints = args
        .clippy_log
        .as_deref()
        .map(external::parse_clippy_log)
        .transpose()?;
    let mutants = args
        .mutants
        .as_deref()
        .map(external::parse_mutants_outcomes)
        .transpose()?;

    if args.write_baseline {
        print!(
            "{}",
            init::baseline_toml(&config, &rust, coverage_pct, crap.as_deref())
        );
        return Ok(());
    }

    let report = report::build(
        &config,
        &rust,
        coverage_pct,
        crap.as_deref(),
        lints.as_ref(),
        mutants.as_ref(),
    );

    if let Some(c) = &cov {
        println!(
            "coverage input: {} instrumented lines\n",
            c.instrumented_line_count()
        );
    }
    println!("{}", report::render_table(&report));
    if !report.violations.is_empty() {
        eprintln!("quality gate violations:");
        for v in &report.violations {
            eprintln!("  - {v}");
        }
    }

    if let Some(path) = &args.json {
        std::fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    if let Some(path) = &args.github_summary {
        write_github_summary(path, &report)?;
    }

    if !report.gate_passed() {
        std::process::exit(1);
    }
    Ok(())
}

fn write_github_summary(path: &std::path::Path, report: &report::Report) -> Result<()> {
    let mut md = String::from("### rust-oleum quality gate\n\n");
    md.push_str("| Metric | Value | Target | Target met | Gate |\n|---|---|---|---|---|\n");
    for row in &report.rows {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            row.metric,
            row.value,
            row.target,
            if row.meets_target { "✅" } else { "⚠️" },
            if row.passes_gate { "✅" } else { "❌" },
        ));
    }
    if !report.violations.is_empty() {
        md.push_str("\n**Violations:**\n");
        for v in &report.violations {
            md.push_str(&format!("- {v}\n"));
        }
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(md.as_bytes())?;
    Ok(())
}
