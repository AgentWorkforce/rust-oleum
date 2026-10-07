//! `rust-oleum init` — detect source roots, measure the project, and write
//! a starter config whose `[baseline]` grandfathers today's violations so
//! the gate can be enforced in CI immediately.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::{config, coverage, rust_metrics, ts_metrics};

pub const CONFIG_FILE: &str = "rust-oleum.toml";

const HEADER: &str = "\
# rust-oleum configuration — https://github.com/AgentWorkforce/rust-oleum
#
# [targets] are the aspirational limits every metric is reported against.
# [baseline] grandfathers existing violations at their current ceiling so
# the gate can be enforced today: new violations, and regressions beyond a
# grandfathered ceiling, fail CI. Shrink the baseline as code improves
# (`rust-oleum --write-baseline` regenerates it); never grow it.
";

const DEFAULT_TARGETS: &str = "\
[targets]
cyclomatic_max = 22
cognitive_max = 22
halstead_difficulty_max = 80.0
file_loc_max = 500
coverage_min_pct = 100.0
crap_max = 25.0
surviving_mutants_max = 0
dead_code_max = 0
redundant_code_max = 0
ts_any_max = 0
ts_unknown_max = 0
";

pub fn run(repo_root: &Path, coverage_file: Option<&Path>, force: bool) -> Result<()> {
    let path = repo_root.join(CONFIG_FILE);
    if path.exists() && !force {
        bail!("{CONFIG_FILE} already exists; pass --force to regenerate it");
    }

    let (rust_roots, ts_roots) = detect_roots(repo_root)?;
    if rust_roots.is_empty() && ts_roots.is_empty() {
        bail!("no source roots found under {} (looked for src/ directories next to Cargo.toml / package.json)", repo_root.display());
    }

    let mut text = format!(
        "{HEADER}\n{DEFAULT_TARGETS}\n[sources]\nrust_roots = {}\nts_roots = {}\n\n",
        toml_array(&rust_roots),
        toml_array(&ts_roots),
    );
    let cfg: config::Config = toml::from_str(&text).context("building default config")?;

    let rust = rust_metrics::collect(repo_root, &cfg.sources.rust_roots)?;
    let ts = ts_metrics::collect(repo_root, &cfg.sources.ts_roots)?;
    let cov = coverage_file
        .map(|p| coverage::Coverage::from_lcov(p, repo_root))
        .transpose()?;
    let coverage_pct = cov.as_ref().map(coverage::Coverage::total_line_coverage);
    let crap = cov.as_ref().map(|c| c.crap_scores(&rust.functions));

    let baseline = baseline_toml(&cfg, &rust, &ts, coverage_pct, crap.as_deref());
    let grandfathered = baseline.lines().filter(|l| l.contains('=')).count();
    text.push_str(&baseline);
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;

    println!("wrote {CONFIG_FILE}");
    println!("  rust roots: {}", rust_roots.join(", "));
    if !ts_roots.is_empty() {
        println!("  ts roots:   {}", ts_roots.join(", "));
    }
    println!(
        "  measured {} files, {} functions; grandfathered {} baseline entries",
        rust.files.len(),
        rust.functions.len(),
        grandfathered,
    );
    println!("run `rust-oleum` to check the gate.");
    Ok(())
}

fn toml_array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
    format!("[{}]", quoted.join(", "))
}

/// Source roots: `src/` directories that sit next to a `Cargo.toml`
/// (Rust) or a `package.json`/`tsconfig.json` (TypeScript) and contain
/// matching sources.
fn detect_roots(repo_root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let mut rust = Vec::new();
    let mut ts = Vec::new();
    let mut stack = vec![repo_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))?
        {
            let path = entry?.path();
            if !path.is_dir() || skip_dir(&path) {
                continue;
            }
            if path.file_name().is_some_and(|n| n == "src") {
                classify_src(&path, repo_root, &mut rust, &mut ts);
            } else {
                stack.push(path);
            }
        }
    }
    rust.sort();
    ts.sort();
    Ok((rust, ts))
}

fn skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "dist" | "vendor")
}

fn classify_src(src: &Path, repo_root: &Path, rust: &mut Vec<String>, ts: &mut Vec<String>) {
    let Some(parent) = src.parent() else { return };
    let rel = src
        .strip_prefix(repo_root)
        .unwrap_or(src)
        .to_string_lossy()
        .replace('\\', "/");
    if parent.join("Cargo.toml").exists() && contains_ext(src, &["rs"]) {
        rust.push(rel.clone());
    }
    if (parent.join("package.json").exists() || parent.join("tsconfig.json").exists())
        && contains_ext(src, &["ts", "mts", "cts"])
    {
        ts.push(rel);
    }
}

fn contains_ext(dir: &Path, exts: &[&str]) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !skip_dir(&path) && contains_ext(&path, exts) {
                return true;
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            return true;
        }
    }
    false
}

/// Render a `[baseline]` section covering every current target violation.
pub fn baseline_toml(
    config: &config::Config,
    rust: &rust_metrics::RustMetrics,
    ts: &ts_metrics::TsTypeCounts,
    coverage_pct: Option<f64>,
    crap: Option<&[coverage::CrapScore]>,
) -> String {
    use std::fmt::Write;
    let t = &config.targets;
    let mut out = String::from("[baseline]\n");
    if let Some(pct) = coverage_pct {
        if pct < t.coverage_min_pct {
            // Floor slightly below the measured value so unrelated PRs don't
            // flap on fractional coverage noise.
            let _ = writeln!(out, "coverage_min_pct = {:.1}", (pct - 0.5).max(0.0));
        }
    }
    if ts.unknown_count > t.ts_unknown_max {
        let _ = writeln!(out, "ts_unknown_max = {}", ts.unknown_count);
    }

    let mut section = |name: &str, entries: Vec<(String, String)>| {
        if entries.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n[baseline.{name}]");
        for (k, v) in entries {
            let _ = writeln!(out, "\"{k}\" = {v}");
        }
    };

    section(
        "file_loc",
        rust.files
            .iter()
            .filter(|f| f.loc > t.file_loc_max)
            .map(|f| (f.file.clone(), f.loc.to_string()))
            .collect(),
    );
    section(
        "cyclomatic",
        rust.functions
            .iter()
            .filter(|f| f.cyclomatic > t.cyclomatic_max)
            .map(|f| {
                (
                    config::function_key(&f.file, &f.name),
                    f.cyclomatic.to_string(),
                )
            })
            .collect(),
    );
    section(
        "cognitive",
        rust.functions
            .iter()
            .filter(|f| f.cognitive > t.cognitive_max)
            .map(|f| {
                (
                    config::function_key(&f.file, &f.name),
                    f.cognitive.to_string(),
                )
            })
            .collect(),
    );
    section(
        "halstead",
        rust.functions
            .iter()
            .filter(|f| f.halstead_difficulty > t.halstead_difficulty_max)
            .map(|f| {
                (
                    config::function_key(&f.file, &f.name),
                    format!("{:.1}", f.halstead_difficulty + 0.1),
                )
            })
            .collect(),
    );
    if let Some(scores) = crap {
        section(
            "crap",
            scores
                .iter()
                .filter(|s| s.crap > t.crap_max)
                .map(|s| {
                    (
                        config::function_key(&s.file, &s.name),
                        format!("{:.1}", s.crap + 0.1),
                    )
                })
                .collect(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_project(root: &Path) {
        let w = |rel: &str, content: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };
        w("crates/a/Cargo.toml", "[package]\nname = \"a\"\n");
        w("crates/a/src/lib.rs", "fn tidy() {}\n");
        w("packages/p/package.json", "{}\n");
        w("packages/p/src/i.ts", "let n: number = 1;\n");
        // Decoys that must not become roots.
        w("crates/a/target/src/gen.rs", "fn t() {}\n");
        w("docs/src/guide.md", "# not code\n");
        w("plain/src/loose.rs", "fn no_manifest_next_door() {}\n");
    }

    #[test]
    fn detects_manifest_adjacent_src_roots() {
        let dir = tempfile::tempdir().unwrap();
        fake_project(dir.path());
        let (rust, ts) = detect_roots(dir.path()).unwrap();
        assert_eq!(rust, vec!["crates/a/src"]);
        assert_eq!(ts, vec!["packages/p/src"]);
    }

    #[test]
    fn init_writes_a_loadable_config_with_baseline() {
        let dir = tempfile::tempdir().unwrap();
        fake_project(dir.path());
        // A file over the LOC target must land in the baseline.
        let big: String = (0..501).map(|i| format!("fn f{i}() {{}}\n")).collect();
        std::fs::write(dir.path().join("crates/a/src/big.rs"), big).unwrap();

        run(dir.path(), None, false).unwrap();
        let config = config::Config::load(&dir.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(config.targets.cyclomatic_max, 22);
        assert_eq!(config.sources.rust_roots, vec!["crates/a/src"]);
        assert_eq!(
            config.baseline.file_loc.get("crates/a/src/big.rs"),
            Some(&501)
        );

        // Refuses to clobber without --force, allows it with.
        assert!(run(dir.path(), None, false).is_err());
        run(dir.path(), None, true).unwrap();
    }
}
