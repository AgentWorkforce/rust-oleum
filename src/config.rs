//! `rust-oleum.toml` — target thresholds plus a grandfathered baseline.
//!
//! `[targets]` holds the aspirational limits the benchmark reports against.
//! `[baseline]` lists existing violations (with their allowed ceiling) so the
//! CI gate passes today while forbidding *new* violations and regressions of
//! grandfathered ones. Shrink the baseline over time; never grow it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub targets: Targets,
    #[serde(default)]
    pub baseline: Baseline,
    pub sources: Sources,
}

/// Every target has a default, so a `[targets]` table only needs the
/// limits it wants to change, and configs written before a metric existed
/// keep loading.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Targets {
    #[serde(default = "defaults::complexity_max")]
    pub cyclomatic_max: u32,
    #[serde(default = "defaults::complexity_max")]
    pub cognitive_max: u32,
    #[serde(default = "defaults::halstead_difficulty_max")]
    pub halstead_difficulty_max: f64,
    #[serde(default = "defaults::file_loc_max")]
    pub file_loc_max: usize,
    #[serde(default = "defaults::coverage_min_pct")]
    pub coverage_min_pct: f64,
    #[serde(default = "defaults::crap_max")]
    pub crap_max: f64,
    #[serde(default)]
    pub surviving_mutants_max: usize,
    #[serde(default)]
    pub dead_code_max: usize,
    #[serde(default)]
    pub redundant_code_max: usize,
    /// `unsafe` blocks (plus `unsafe fn`) per function.
    #[serde(default)]
    pub unsafe_max: u32,
    /// `.unwrap()` / `.expect()` / panicking macros per function.
    #[serde(default)]
    pub panic_max: u32,
    /// Lints suppressed by `#[allow]` / `#[expect]` per function.
    #[serde(default)]
    pub allow_max: u32,
    /// Lints suppressed outside functions, per file.
    #[serde(default)]
    pub file_allow_max: u32,
}

impl Default for Targets {
    fn default() -> Self {
        Self {
            cyclomatic_max: defaults::complexity_max(),
            cognitive_max: defaults::complexity_max(),
            halstead_difficulty_max: defaults::halstead_difficulty_max(),
            file_loc_max: defaults::file_loc_max(),
            coverage_min_pct: defaults::coverage_min_pct(),
            crap_max: defaults::crap_max(),
            surviving_mutants_max: 0,
            dead_code_max: 0,
            redundant_code_max: 0,
            unsafe_max: 0,
            panic_max: 0,
            allow_max: 0,
            file_allow_max: 0,
        }
    }
}

/// Documented default targets (zero-valued ones use `Default::default`).
mod defaults {
    pub fn complexity_max() -> u32 {
        22
    }
    pub fn halstead_difficulty_max() -> f64 {
        80.0
    }
    pub fn file_loc_max() -> usize {
        500
    }
    pub fn coverage_min_pct() -> f64 {
        100.0
    }
    pub fn crap_max() -> f64 {
        25.0
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    /// Enforced coverage floor while below the 100% target.
    pub coverage_min_pct: Option<f64>,
    /// file path -> allowed LOC ceiling.
    #[serde(default)]
    pub file_loc: BTreeMap<String, usize>,
    /// "file::function" -> allowed cyclomatic ceiling.
    #[serde(default)]
    pub cyclomatic: BTreeMap<String, u32>,
    /// "file::function" -> allowed cognitive ceiling.
    #[serde(default)]
    pub cognitive: BTreeMap<String, u32>,
    /// "file::function" -> allowed Halstead-difficulty ceiling.
    #[serde(default)]
    pub halstead: BTreeMap<String, f64>,
    /// "file::function" -> allowed CRAP ceiling.
    #[serde(default)]
    pub crap: BTreeMap<String, f64>,
    /// "file::function" -> allowed `unsafe` count.
    #[serde(default, rename = "unsafe")]
    pub unsafe_: BTreeMap<String, u32>,
    /// "file::function" -> allowed panic-call count.
    #[serde(default)]
    pub panic: BTreeMap<String, u32>,
    /// "file::function" -> allowed lint-suppression count.
    #[serde(default)]
    pub allow: BTreeMap<String, u32>,
    /// file path -> allowed file-level lint-suppression count.
    #[serde(default)]
    pub file_allow: BTreeMap<String, u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    /// Roots scanned for production Rust code.
    pub rust_roots: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Key used for per-function baseline entries.
pub fn function_key(file: &str, name: &str) -> String {
    format!("{file}::{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_key_is_path_and_name() {
        assert_eq!(function_key("src/a.rs", "S::f"), "src/a.rs::S::f");
    }

    #[test]
    fn loads_the_repo_config() {
        // The checked-in rust-oleum.toml must always parse; baseline sections
        // are optional but targets and sources are required.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("rust-oleum.toml");
        let config = Config::load(&root).unwrap();
        assert!(config.targets.cyclomatic_max > 0);
        assert!(!config.sources.rust_roots.is_empty());
    }

    #[test]
    fn partial_targets_fall_back_to_documented_defaults() {
        let config: Config =
            toml::from_str("[targets]\ncyclomatic_max = 10\n[sources]\nrust_roots = [\"src\"]\n")
                .unwrap();
        let t = &config.targets;
        assert_eq!(t.cyclomatic_max, 10);
        assert_eq!(t.cognitive_max, 22);
        assert_eq!(t.halstead_difficulty_max, 80.0);
        assert_eq!(t.file_loc_max, 500);
        assert_eq!(t.coverage_min_pct, 100.0);
        assert_eq!(t.crap_max, 25.0);
        assert_eq!(t.surviving_mutants_max, 0);
        assert_eq!(t.dead_code_max, 0);
        assert_eq!(t.redundant_code_max, 0);
        assert_eq!(t.unsafe_max, 0);
        assert_eq!(t.panic_max, 0);
        assert_eq!(t.allow_max, 0);
        assert_eq!(t.file_allow_max, 0);
        let d = Targets::default();
        assert_eq!(
            (d.cyclomatic_max, d.file_loc_max, d.crap_max),
            (22, 500, 25.0)
        );
    }

    #[test]
    fn safety_baselines_load() {
        let config: Config = toml::from_str(
            r#"
            [targets]
            [sources]
            rust_roots = ["src"]
            [baseline.unsafe]
            "src/a.rs::f" = 2
            [baseline.panic]
            "src/a.rs::g" = 3
            [baseline.allow]
            "src/a.rs::h" = 1
            [baseline.file_allow]
            "src/a.rs" = 4
            "#,
        )
        .unwrap();
        let b = &config.baseline;
        assert_eq!(b.unsafe_.get("src/a.rs::f"), Some(&2));
        assert_eq!(b.panic.get("src/a.rs::g"), Some(&3));
        assert_eq!(b.allow.get("src/a.rs::h"), Some(&1));
        assert_eq!(b.file_allow.get("src/a.rs"), Some(&4));
    }

    #[test]
    fn unknown_target_is_still_rejected() {
        let err =
            toml::from_str::<Config>("[targets]\nbogus_max = 1\n[sources]\nrust_roots = []\n");
        assert!(err.is_err());
    }
}
