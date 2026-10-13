//! `rust-oleum init` — detect source roots, measure the project, and write
//! a starter config whose `[baseline]` grandfathers today's violations so
//! the gate can be enforced in CI immediately.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::{config, coverage, report, rust_metrics};

pub const CONFIG_FILE: &str = "rust-oleum.toml";

const HEADER: &str = "\
# rust-oleum configuration — https://github.com/AgentWorkforce/rust-oleum
#
# [targets] are the aspirational limits every metric is reported against.
# [baseline] grandfathers existing violations at their current ceiling so
# the gate can be enforced today: new violations, and regressions beyond a
# grandfathered ceiling, fail CI. So do loose or stale entries: when code
# improves, `rust-oleum --tighten` shrinks this section in place.
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
unsafe_max = 0
panic_max = 0
allow_max = 0
file_allow_max = 0
";

pub fn run(repo_root: &Path, coverage_file: Option<&Path>, force: bool) -> Result<()> {
    let path = repo_root.join(CONFIG_FILE);
    if path.exists() && !force {
        bail!("{CONFIG_FILE} already exists; pass --force to regenerate it");
    }

    let rust_roots = detect_roots(repo_root)?;
    if rust_roots.is_empty() {
        bail!(
            "no source roots found under {} (looked for src/ directories next to a Cargo.toml)",
            repo_root.display()
        );
    }

    let mut text = format!(
        "{HEADER}\n{DEFAULT_TARGETS}\n[sources]\nrust_roots = {}\n\n",
        toml_array(&rust_roots),
    );
    let cfg: config::Config = toml::from_str(&text).context("building default config")?;

    let rust = rust_metrics::collect(repo_root, &cfg.sources.rust_roots)?;
    let cov = coverage_file
        .map(|p| coverage::Coverage::from_lcov(p, repo_root))
        .transpose()?;
    let coverage_pct = cov.as_ref().map(coverage::Coverage::total_line_coverage);
    let crap = cov.as_ref().map(|c| c.crap_scores(&rust.functions));

    let baseline = baseline_toml(&cfg, &rust, coverage_pct, crap.as_deref());
    let grandfathered = baseline.lines().filter(|l| l.contains('=')).count();
    text.push_str(&baseline);
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;

    println!("wrote {CONFIG_FILE}");
    println!("  rust roots: {}", rust_roots.join(", "));
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
/// and contain Rust sources.
fn detect_roots(repo_root: &Path) -> Result<Vec<String>> {
    let mut rust = Vec::new();
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
                if let Some(rel) = rust_src_root(&path, repo_root) {
                    rust.push(rel);
                }
            } else {
                stack.push(path);
            }
        }
    }
    rust.sort();
    Ok(rust)
}

fn skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    name.starts_with('.') || matches!(name, "target" | "node_modules" | "dist" | "vendor")
}

fn rust_src_root(src: &Path, repo_root: &Path) -> Option<String> {
    let parent = src.parent()?;
    if !parent.join("Cargo.toml").exists() || !contains_ext(src, &["rs"]) {
        return None;
    }
    Some(
        src.strip_prefix(repo_root)
            .unwrap_or(src)
            .to_string_lossy()
            .replace('\\', "/"),
    )
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
    coverage_pct: Option<f64>,
    crap: Option<&[coverage::CrapScore]>,
) -> String {
    render_baseline(&baseline(config, rust, coverage_pct, crap))
}

/// Baseline entries pinning every current target violation at its
/// measured value (floats get one tenth of headroom, coverage half a point).
pub fn baseline(
    config: &config::Config,
    rust: &rust_metrics::RustMetrics,
    coverage_pct: Option<f64>,
    crap: Option<&[coverage::CrapScore]>,
) -> config::Baseline {
    let t = &config.targets;
    let mut b = config::Baseline {
        coverage_min_pct: coverage_pct
            .filter(|pct| *pct < t.coverage_min_pct)
            .map(coverage_floor),
        ..Default::default()
    };
    for f in &rust.files {
        if f.loc > t.file_loc_max {
            insert_max(&mut b.file_loc, f.file.clone(), f.loc);
        }
        if f.allow_count > t.file_allow_max {
            insert_max(&mut b.file_allow, f.file.clone(), f.allow_count);
        }
    }
    for f in &rust.functions {
        let key = || config::function_key(&f.file, &f.name);
        if f.cyclomatic > t.cyclomatic_max {
            insert_max(&mut b.cyclomatic, key(), f.cyclomatic);
        }
        if f.cognitive > t.cognitive_max {
            insert_max(&mut b.cognitive, key(), f.cognitive);
        }
        if f.halstead_difficulty > t.halstead_difficulty_max {
            insert_max(&mut b.halstead, key(), float_ceiling(f.halstead_difficulty));
        }
        if f.unsafe_count > t.unsafe_max {
            insert_max(&mut b.unsafe_, key(), f.unsafe_count);
        }
        if f.panic_count > t.panic_max {
            insert_max(&mut b.panic, key(), f.panic_count);
        }
        if f.allow_count > t.allow_max {
            insert_max(&mut b.allow, key(), f.allow_count);
        }
    }
    for s in crap
        .unwrap_or_default()
        .iter()
        .filter(|s| s.crap > t.crap_max)
    {
        let key = config::function_key(&s.file, &s.name);
        insert_max(&mut b.crap, key, float_ceiling(s.crap));
    }
    b
}

/// Ceiling for a one-decimal float metric: the displayed value plus a tenth,
/// so the gate's stale-ceiling check (one tenth of slack) accepts it.
pub fn float_ceiling(x: f64) -> f64 {
    (report::tenths(x) + 1) as f64 / 10.0
}

/// Coverage floor slightly below the measured value so unrelated PRs don't
/// flap on fractional coverage noise.
pub fn coverage_floor(pct: f64) -> f64 {
    report::tenths((pct - 0.5).max(0.0)) as f64 / 10.0
}

fn insert_max<V: PartialOrd + Copy>(map: &mut BTreeMap<String, V>, key: String, value: V) {
    let slot = map.entry(key).or_insert(value);
    if value > *slot {
        *slot = value;
    }
}

/// Render a baseline as the `[baseline]` TOML section (and its subtables).
pub fn render_baseline(b: &config::Baseline) -> String {
    use std::fmt::Write;
    let mut out = String::from("[baseline]\n");
    if let Some(floor) = b.coverage_min_pct {
        let _ = writeln!(out, "coverage_min_pct = {floor:?}");
    }
    section(&mut out, "file_loc", &b.file_loc, |v| v.to_string());
    section(&mut out, "cyclomatic", &b.cyclomatic, |v| v.to_string());
    section(&mut out, "cognitive", &b.cognitive, |v| v.to_string());
    section(&mut out, "halstead", &b.halstead, |v| format!("{v:?}"));
    section(&mut out, "crap", &b.crap, |v| format!("{v:?}"));
    section(&mut out, "unsafe", &b.unsafe_, |v| v.to_string());
    section(&mut out, "panic", &b.panic, |v| v.to_string());
    section(&mut out, "allow", &b.allow, |v| v.to_string());
    section(&mut out, "file_allow", &b.file_allow, |v| v.to_string());
    out
}

fn section<V>(out: &mut String, name: &str, entries: &BTreeMap<String, V>, fmt: fn(&V) -> String) {
    use std::fmt::Write;
    if entries.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n[baseline.{name}]");
    for (k, v) in entries {
        let key = k.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = writeln!(out, "\"{key}\" = {}", fmt(v));
    }
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
        // Decoys that must not become roots.
        w("crates/a/target/src/gen.rs", "fn t() {}\n");
        w("docs/src/guide.md", "# not code\n");
        w("plain/src/loose.rs", "fn no_manifest_next_door() {}\n");
    }

    #[test]
    fn detects_manifest_adjacent_src_roots() {
        let dir = tempfile::tempdir().unwrap();
        fake_project(dir.path());
        let rust = detect_roots(dir.path()).unwrap();
        assert_eq!(rust, vec!["crates/a/src"]);
    }

    #[test]
    fn init_writes_a_loadable_config_with_baseline() {
        let dir = tempfile::tempdir().unwrap();
        fake_project(dir.path());
        // A file over the LOC target must land in the baseline.
        let big: String = (0..501).map(|i| format!("fn f{i}() {{}}\n")).collect();
        std::fs::write(dir.path().join("crates/a/src/big.rs"), big).unwrap();
        // Safety offenders must be grandfathered too.
        std::fs::write(
            dir.path().join("crates/a/src/risky.rs"),
            "#![allow(unused)]\n#[allow(dead_code)]\nunsafe fn r() { unsafe { x() }; y.unwrap(); }\n",
        )
        .unwrap();

        run(dir.path(), None, false).unwrap();
        let config = config::Config::load(&dir.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(config.targets.cyclomatic_max, 22);
        assert_eq!(config.sources.rust_roots, vec!["crates/a/src"]);
        assert_eq!(
            config.baseline.file_loc.get("crates/a/src/big.rs"),
            Some(&501)
        );
        let b = &config.baseline;
        assert_eq!(b.unsafe_.get("crates/a/src/risky.rs::r"), Some(&2));
        assert_eq!(b.panic.get("crates/a/src/risky.rs::r"), Some(&1));
        assert_eq!(b.allow.get("crates/a/src/risky.rs::r"), Some(&1));
        assert_eq!(b.file_allow.get("crates/a/src/risky.rs"), Some(&1));
        assert!(!b.unsafe_.contains_key("crates/a/src/lib.rs::tidy"));

        // Refuses to clobber without --force, allows it with.
        assert!(run(dir.path(), None, false).is_err());
        run(dir.path(), None, true).unwrap();
    }

    fn func(name: &str, cx: u32, halstead: f64) -> rust_metrics::FunctionMetrics {
        rust_metrics::FunctionMetrics {
            file: "src/a.rs".into(),
            name: name.into(),
            start_line: 1,
            end_line: 2,
            cyclomatic: cx,
            cognitive: cx,
            halstead_difficulty: halstead,
            unsafe_count: 0,
            panic_count: 0,
            allow_count: 0,
        }
    }

    /// Paste `baseline_toml` output back into a config and gate the same
    /// measurements: it must pass with no stale, loose, or unmatched entries.
    fn assert_round_trip(rust: &rust_metrics::RustMetrics, pct: f64, crap: &[coverage::CrapScore]) {
        let base = format!("{DEFAULT_TARGETS}\n[sources]\nrust_roots = [\"src\"]\n\n");
        let cfg: config::Config = toml::from_str(&base).unwrap();
        let text = format!("{base}{}", baseline_toml(&cfg, rust, Some(pct), Some(crap)));
        let cfg: config::Config = toml::from_str(&text).unwrap();
        let report = report::build(&cfg, rust, Some(pct), Some(crap), None, None);
        assert!(report.gate_passed(), "{text}\n{:?}", report.violations);
    }

    #[test]
    fn write_baseline_output_round_trips_through_the_gate() {
        let functions: Vec<_> = (0..2000)
            .map(|i| func(&format!("f{i}"), 23 + i % 7, 80.0 + f64::from(i) * 0.0137))
            .collect();
        let crap: Vec<_> = functions
            .iter()
            .enumerate()
            .map(|(i, f)| coverage::CrapScore {
                file: f.file.clone(),
                name: f.name.clone(),
                cyclomatic: f.cyclomatic,
                coverage_pct: 0.0,
                crap: 25.0 + i as f64 * 0.0451,
            })
            .collect();
        let mut rust = rust_metrics::RustMetrics {
            files: vec![rust_metrics::FileMetrics {
                file: "src/a.rs".into(),
                loc: 812,
                allow_count: 0,
            }],
            functions,
        };
        // Keys needing TOML escaping survive, and duplicate keys keep the max.
        rust.functions.push(func("Q<\"x\\y\">::f", 30, 90.0));
        rust.functions.push(func("dup", 30, 81.0));
        rust.functions.push(func("dup", 25, 95.0));
        assert_round_trip(&rust, 73.21, &crap);

        let empty = rust_metrics::RustMetrics::default();
        for i in 0..=2_000 {
            assert_round_trip(&empty, f64::from(i) * 0.0497, &[]);
        }
    }
}
