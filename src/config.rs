//! `rust-oleum.toml` — target thresholds plus a grandfathered baseline.
//!
//! `[targets]` holds the aspirational limits the benchmark reports against.
//! `[baseline]` lists existing violations (with their allowed ceiling) so the
//! CI gate passes today while forbidding *new* violations and regressions of
//! grandfathered ones. Shrink the baseline over time; never grow it.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub targets: Targets,
    /// Clippy/rustc lint groups, gated per item like dead code. Without a
    /// `[lint_groups]` section the BeautifulRust groups apply; any
    /// `[lint_groups]` table, even an empty one, replaces them.
    #[serde(default = "crate::presets::default_lint_groups")]
    pub lint_groups: BTreeMap<String, LintGroup>,
    #[serde(default)]
    pub baseline: Baseline,
    pub sources: Sources,
}

/// A named set of lints ratcheted together: `[lint_groups.<name>]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintGroup {
    /// Lint codes as they appear in the clippy log (`clippy::unwrap_used`,
    /// `missing_docs`); a trailing `*` matches by prefix.
    pub lints: Vec<String>,
    /// Allowed findings per item (lint + function).
    #[serde(default)]
    pub max: usize,
}

/// Every target has a default, so a `[targets]` table only needs the
/// limits it wants to change, and configs written before a metric existed
/// keep loading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Allowed dead-code findings per item (lint + function).
    #[serde(default)]
    pub dead_code_max: usize,
    /// Allowed redundant-code findings per item (lint + function).
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

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
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
    /// "lint@file::function" (or "lint@file#Lline") -> allowed finding count.
    #[serde(default)]
    pub dead_code: BTreeMap<String, usize>,
    /// "lint@file::function" (or "lint@file#Lline") -> allowed finding count.
    #[serde(default)]
    pub redundant_code: BTreeMap<String, usize>,
    /// Lint group name -> "lint@file::function" (or "lint@file#Lline") ->
    /// allowed finding count.
    #[serde(default)]
    pub lint_groups: BTreeMap<String, BTreeMap<String, usize>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    /// Roots scanned for production Rust code.
    pub rust_roots: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Parse and validate a config document.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.validate().context("validating config")?;
        Ok(config)
    }

    /// Lint groups must have plain names and well-formed patterns, no lint
    /// may match two groups, and every baselined group must be defined.
    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_numeric()?;
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for (name, group) in &self.lint_groups {
            if !is_group_name(name) {
                bail!(
                    "lint group name `{name}` must be non-empty ASCII letters, digits, `_` or `-`"
                );
            }
            for pattern in &group.lints {
                check_pattern(name, pattern)?;
                if let Some((other, p)) = seen.iter().find(|(_, p)| patterns_overlap(p, pattern)) {
                    bail!(
                        "lint pattern `{pattern}` in [lint_groups.{name}] overlaps `{p}` in \
                         [lint_groups.{other}]; a lint may belong to one group only"
                    );
                }
                seen.push((name, pattern));
            }
        }
        if let Some(name) = self
            .baseline
            .lint_groups
            .keys()
            .find(|n| !self.lint_groups.contains_key(*n))
        {
            bail!("[baseline.lint_groups.{name}] has no matching [lint_groups.{name}] definition");
        }
        Ok(())
    }

    /// Reject values that make comparisons meaningless or disable the gate.
    fn validate_numeric(&self) -> Result<()> {
        validate_percentage("targets.coverage_min_pct", self.targets.coverage_min_pct)?;
        if let Some(floor) = self.baseline.coverage_min_pct {
            validate_percentage("baseline.coverage_min_pct", floor)?;
        }
        for (name, value) in [
            (
                "targets.halstead_difficulty_max",
                self.targets.halstead_difficulty_max,
            ),
            ("targets.crap_max", self.targets.crap_max),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, entries) in [
            ("baseline.halstead", &self.baseline.halstead),
            ("baseline.crap", &self.baseline.crap),
        ] {
            for (key, value) in entries {
                validate_nonnegative(&format!("{name}[{key:?}]"), *value)?;
            }
        }
        Ok(())
    }

    /// The configured lint group a lint code belongs to, if any.
    pub fn lint_group_of(&self, code: &str) -> Option<&str> {
        self.lint_groups
            .iter()
            .find(|(_, g)| g.lints.iter().any(|p| pattern_matches(p, code)))
            .map(|(name, _)| name.as_str())
    }
}

fn is_group_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A pattern is a lint name, optionally ending in one `*` wildcard.
fn check_pattern(group: &str, pattern: &str) -> Result<()> {
    let stem = pattern.strip_suffix('*').unwrap_or(pattern);
    if stem.is_empty() || stem.contains('*') || stem.contains(char::is_whitespace) {
        bail!(
            "lint pattern `{pattern}` in [lint_groups.{group}] must be a lint name, \
             optionally ending in `*`"
        );
    }
    Ok(())
}

/// Exact match, or prefix match for a pattern ending in `*`.
pub fn pattern_matches(pattern: &str, code: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => code.starts_with(prefix),
        None => pattern == code,
    }
}

/// Whether some lint code could match both patterns.
fn patterns_overlap(a: &str, b: &str) -> bool {
    match (a.strip_suffix('*'), b.strip_suffix('*')) {
        (Some(pa), Some(pb)) => pa.starts_with(pb) || pb.starts_with(pa),
        (Some(prefix), None) => b.starts_with(prefix),
        (None, Some(prefix)) => a.starts_with(prefix),
        (None, None) => a == b,
    }
}

fn validate_nonnegative(name: &str, value: f64) -> Result<()> {
    ensure!(
        value.is_finite() && value >= 0.0,
        "{name} must be finite and nonnegative, got {value}"
    );
    Ok(())
}

fn validate_percentage(name: &str, value: f64) -> Result<()> {
    validate_nonnegative(name, value)?;
    ensure!(
        value <= 100.0,
        "{name} must be between 0 and 100, got {value}"
    );
    Ok(())
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

    const GROUPS: &str = r#"
        [targets]
        [sources]
        rust_roots = ["src"]
        [lint_groups.panics]
        lints = ["clippy::unwrap_used", "clippy::panic*"]
        max = 2
        [lint_groups.docs]
        lints = ["missing_docs", "clippy::missing_*"]
        [baseline.lint_groups.panics]
        "clippy::unwrap_used@src/a.rs::f" = 3
        "#;

    #[test]
    fn beautiful_rust_groups_are_the_default_and_any_table_replaces_them() {
        let bare = "[targets]\n[sources]\nrust_roots = []\n";
        let config = Config::parse(bare).unwrap();
        assert_eq!(config.lint_groups.len(), 7);
        assert_eq!(config.lint_group_of("clippy::unwrap_used"), Some("panics"));
        let opted_out = Config::parse(&format!("{bare}[lint_groups]\n")).unwrap();
        assert!(opted_out.lint_groups.is_empty());
        let own = Config::parse(&format!("{bare}[lint_groups.x]\nlints = [\"y\"]\n")).unwrap();
        assert_eq!(own.lint_groups.keys().collect::<Vec<_>>(), ["x"]);
        assert_eq!(own.lint_group_of("clippy::unwrap_used"), None);
    }

    #[test]
    fn lint_groups_load_with_baselines() {
        let config = Config::parse(GROUPS).unwrap();
        let panics = &config.lint_groups["panics"];
        assert_eq!(panics.max, 2);
        assert_eq!(config.lint_groups["docs"].max, 0);
        assert_eq!(
            config.baseline.lint_groups["panics"].get("clippy::unwrap_used@src/a.rs::f"),
            Some(&3)
        );
        assert_eq!(config.lint_group_of("clippy::unwrap_used"), Some("panics"));
        assert_eq!(
            config.lint_group_of("clippy::panic_in_result_fn"),
            Some("panics")
        );
        assert_eq!(config.lint_group_of("missing_docs"), Some("docs"));
        assert_eq!(
            config.lint_group_of("clippy::missing_errors_doc"),
            Some("docs")
        );
        assert_eq!(config.lint_group_of("clippy::missing"), None);
        assert_eq!(config.lint_group_of("clippy::unwrap_or_default"), None);
        assert_eq!(config.lint_group_of("missing_docs_x"), None);
    }

    fn parse_err(extra: &str) -> String {
        let text = format!("[targets]\n[sources]\nrust_roots = []\n{extra}");
        format!("{:#}", Config::parse(&text).unwrap_err())
    }

    #[test]
    fn invalid_lint_groups_are_rejected() {
        let e = parse_err("[lint_groups.a]\nlints = [\"x\", \"clippy::y*\"]\n[lint_groups.b]\nlints = [\"clippy::y_z\"]\n");
        assert!(
            e.contains("`clippy::y_z` in [lint_groups.b] overlaps `clippy::y*` in [lint_groups.a]"),
            "{e}"
        );
        // Within one group a duplicate is an overlap too.
        assert!(parse_err("[lint_groups.a]\nlints = [\"x\", \"x\"]\n").contains("overlaps"));
        for bad in ["*", "a*b", "a**", "", "a b"] {
            let e = parse_err(&format!("[lint_groups.a]\nlints = [\"{bad}\"]\n"));
            assert!(e.contains("optionally ending in `*`"), "{bad}: {e}");
        }
        let e = parse_err("[lint_groups.\"a.b\"]\nlints = []\n");
        assert!(e.contains("lint group name `a.b`"), "{e}");
        assert!(parse_err("[lint_groups.\"\"]\nlints = []\n").contains("name ``"));
        let e = parse_err("[baseline.lint_groups.ghost]\n\"k\" = 1\n");
        assert!(
            e.contains("[baseline.lint_groups.ghost] has no matching"),
            "{e}"
        );
        assert!(parse_err("[lint_groups.a]\nlints = []\nbogus = 1\n").contains("bogus"));
    }

    #[test]
    fn pattern_overlap_covers_every_shape() {
        assert!(patterns_overlap("a*", "ab*"));
        assert!(patterns_overlap("ab*", "a*"));
        assert!(!patterns_overlap("ab*", "ac*"));
        assert!(patterns_overlap("a*", "ab"));
        assert!(patterns_overlap("ab", "a*"));
        assert!(!patterns_overlap("b", "a*"));
        assert!(!patterns_overlap("a*", "b"));
        assert!(patterns_overlap("a", "a"));
        assert!(!patterns_overlap("a", "b"));
        assert!(is_group_name("unsafe_hygiene-2"));
        assert!(!is_group_name("a b"));
    }

    #[test]
    fn unknown_target_is_still_rejected() {
        let err =
            toml::from_str::<Config>("[targets]\nbogus_max = 1\n[sources]\nrust_roots = []\n");
        assert!(err.is_err());
    }
    #[test]
    fn numeric_policy_ranges_include_boundaries_and_reject_nonfinite_values() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1] {
            assert!(validate_nonnegative("metric", value).is_err());
            assert!(validate_percentage("coverage", value).is_err());
        }
        for value in [0.0, 50.0, 100.0] {
            assert!(validate_percentage("coverage", value).is_ok());
        }
        assert!(validate_percentage("coverage", 100.01).is_err());
        assert!(validate_nonnegative("metric", 100.01).is_ok());
        assert!(validate_nonnegative("metric", 0.0).is_ok());
    }
}
