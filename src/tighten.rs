//! `rust-oleum --tighten` — rewrite the config's `[baseline]` in place so
//! every grandfathered ceiling pins today's measurement.
//!
//! Tightening only ever shrinks the baseline: entries that are no longer
//! needed are removed, loose ceilings are lowered, and regressions or new
//! violations are *not* grandfathered (the gate keeps failing on them).
//! Metrics whose input was not provided keep their entries untouched.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::{Baseline, Config};
use crate::init;

/// What a tighten pass changed.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub removed: usize,
    pub lowered: usize,
    /// Fresh violations or regressions left out of the baseline.
    pub refused: usize,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tightened baseline: {} entries removed, {} lowered",
            self.removed, self.lowered
        )?;
        if self.refused > 0 {
            write!(
                f,
                "; {} new violations or regressions left out (the gate still fails on them)",
                self.refused
            )?;
        }
        Ok(())
    }
}

/// Which optional inputs were measured in this run.
#[derive(Debug, Clone, Copy)]
pub struct Measured {
    pub coverage: bool,
    pub crap: bool,
}

/// Rewrite `path` with `old` clamped to the freshly measured baseline.
pub fn run(path: &Path, old: &Baseline, fresh: &Baseline, measured: Measured) -> Result<Summary> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (tight, summary) = clamp(old, fresh, measured);
    let rewritten = replace_baseline(&text, &init::render_baseline(&tight))?;
    toml::from_str::<Config>(&rewritten).context("tightened config no longer parses")?;
    std::fs::write(path, rewritten).with_context(|| format!("writing {}", path.display()))?;
    Ok(summary)
}

/// Keep only `old` entries, each at the tighter of old and fresh.
pub fn clamp(old: &Baseline, fresh: &Baseline, measured: Measured) -> (Baseline, Summary) {
    let mut s = Summary::default();
    let coverage_min_pct = if measured.coverage {
        clamp_floor(old.coverage_min_pct, fresh.coverage_min_pct, &mut s)
    } else {
        old.coverage_min_pct
    };
    let crap = if measured.crap {
        clamp_map(&old.crap, &fresh.crap, &mut s)
    } else {
        old.crap.clone()
    };
    let tight = Baseline {
        coverage_min_pct,
        file_loc: clamp_map(&old.file_loc, &fresh.file_loc, &mut s),
        cyclomatic: clamp_map(&old.cyclomatic, &fresh.cyclomatic, &mut s),
        cognitive: clamp_map(&old.cognitive, &fresh.cognitive, &mut s),
        halstead: clamp_map(&old.halstead, &fresh.halstead, &mut s),
        crap,
        unsafe_: clamp_map(&old.unsafe_, &fresh.unsafe_, &mut s),
        panic: clamp_map(&old.panic, &fresh.panic, &mut s),
        allow: clamp_map(&old.allow, &fresh.allow, &mut s),
        file_allow: clamp_map(&old.file_allow, &fresh.file_allow, &mut s),
    };
    (tight, s)
}

fn clamp_floor(old: Option<f64>, fresh: Option<f64>, s: &mut Summary) -> Option<f64> {
    match (old, fresh) {
        (Some(_), None) => {
            s.removed += 1;
            None
        }
        (Some(o), Some(n)) if n > o => {
            s.lowered += 1;
            Some(n)
        }
        (None, Some(_)) => {
            s.refused += 1;
            None
        }
        (o, _) => o,
    }
}

fn clamp_map<V: PartialOrd + Copy>(
    old: &BTreeMap<String, V>,
    fresh: &BTreeMap<String, V>,
    s: &mut Summary,
) -> BTreeMap<String, V> {
    let mut out = BTreeMap::new();
    for (key, &o) in old {
        match fresh.get(key) {
            None => s.removed += 1,
            Some(&n) if n < o => {
                s.lowered += 1;
                out.insert(key.clone(), n);
            }
            Some(&n) => {
                if n > o {
                    s.refused += 1;
                }
                out.insert(key.clone(), o);
            }
        }
    }
    s.refused += fresh.keys().filter(|k| !old.contains_key(*k)).count();
    out
}

fn is_baseline_header(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("[baseline]") || line.starts_with("[baseline.")
}

/// Replace everything from the `[baseline]` header to the end of the file
/// with `section`, keeping the bytes before it unchanged. Appends the section
/// when the file has none.
pub fn replace_baseline(text: &str, section: &str) -> Result<String> {
    let mut offset = 0;
    let mut start = None;
    for line in text.split_inclusive('\n') {
        if start.is_none() && is_baseline_header(line) {
            start = Some(offset);
        } else if start.is_some() && line.trim_start().starts_with('[') && !is_baseline_header(line)
        {
            bail!(
                "cannot tighten: `{}` follows [baseline]; move the [baseline] tables to the end of the file",
                line.trim()
            );
        }
        offset += line.len();
    }
    let mut out = match start {
        Some(i) => text[..i].to_string(),
        None => {
            let mut s = text.to_string();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            if !s.is_empty() {
                s.push('\n');
            }
            s
        }
    };
    out.push_str(section);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Measured = Measured {
        coverage: true,
        crap: true,
    };

    fn baseline(entries: &[(&str, u32)]) -> Baseline {
        Baseline {
            cyclomatic: entries.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn clamp_removes_lowers_and_refuses() {
        let old = baseline(&[("a::gone", 30), ("a::better", 40), ("a::worse", 25)]);
        let fresh = baseline(&[("a::better", 31), ("a::worse", 27), ("a::new", 50)]);
        let (tight, s) = clamp(&old, &fresh, ALL);
        assert_eq!(
            tight.cyclomatic,
            BTreeMap::from([("a::better".into(), 31), ("a::worse".into(), 25)])
        );
        assert_eq!(
            s,
            Summary {
                removed: 1,
                lowered: 1,
                refused: 2
            }
        );
        assert!(s
            .to_string()
            .contains("1 entries removed, 1 lowered; 2 new"));
    }

    #[test]
    fn clamp_keeps_unmeasured_metrics() {
        let old = Baseline {
            coverage_min_pct: Some(70.0),
            crap: BTreeMap::from([("a::f".into(), 40.0)]),
            ..Default::default()
        };
        let none = Measured {
            coverage: false,
            crap: false,
        };
        let (tight, s) = clamp(&old, &Baseline::default(), none);
        assert_eq!(tight.coverage_min_pct, Some(70.0));
        assert_eq!(tight.crap.len(), 1);
        assert_eq!(s, Summary::default());
        // Measured and gone: both are removed.
        let (tight, s) = clamp(&old, &Baseline::default(), ALL);
        assert_eq!(tight.coverage_min_pct, None);
        assert!(tight.crap.is_empty());
        assert_eq!(s.removed, 2);
    }

    #[test]
    fn clamp_raises_the_coverage_floor_only() {
        let old = Baseline {
            coverage_min_pct: Some(70.0),
            ..Default::default()
        };
        let higher = Baseline {
            coverage_min_pct: Some(80.0),
            ..Default::default()
        };
        assert_eq!(clamp(&old, &higher, ALL).0.coverage_min_pct, Some(80.0));
        assert_eq!(clamp(&higher, &old, ALL).0.coverage_min_pct, Some(80.0));
        let (tight, s) = clamp(&Baseline::default(), &old, ALL);
        assert_eq!(tight.coverage_min_pct, None);
        assert_eq!(s.refused, 1);
    }

    #[test]
    fn replace_keeps_prefix_bytes_and_appends_when_missing() {
        let prefix = "# keep  me\n[targets]\nx = 1 # odd   spacing\n\n";
        let text =
            format!("{prefix}[baseline]\ncoverage_min_pct = 1.0\n\n[baseline.crap]\n\"a\" = 2.0\n");
        let out = replace_baseline(&text, "[baseline]\n").unwrap();
        assert_eq!(out, format!("{prefix}[baseline]\n"));

        assert_eq!(
            replace_baseline("[targets]\nx = 1", "[baseline]\n").unwrap(),
            "[targets]\nx = 1\n\n[baseline]\n"
        );
        assert!(replace_baseline("[baseline]\n[sources]\n", "[baseline]\n").is_err());
    }

    #[test]
    fn run_rewrites_the_config_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rust-oleum.toml");
        let head = "# my notes\n[targets]\ncyclomatic_max = 22\ncognitive_max = 22\n\
            halstead_difficulty_max = 80.0\nfile_loc_max = 500\ncoverage_min_pct = 100.0\n\
            crap_max = 25.0\nsurviving_mutants_max = 0\ndead_code_max = 0\n\
            redundant_code_max = 0\n\n[sources]\nrust_roots = [\"src\"]\n\n";
        let text = format!(
            "{head}[baseline]\n\n[baseline.cyclomatic]\n\"src/a.rs::gone\" = 40\n\"src/a.rs::f\" = 99\n"
        );
        std::fs::write(&path, &text).unwrap();
        let old = Config::load(&path).unwrap().baseline;
        let fresh = baseline(&[("src/a.rs::f", 30)]);

        let s = run(&path, &old, &fresh, ALL).unwrap();
        assert_eq!((s.removed, s.lowered, s.refused), (1, 1, 0));
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            format!("{head}[baseline]\n\n[baseline.cyclomatic]\n\"src/a.rs::f\" = 30\n")
        );
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(reloaded.baseline.cyclomatic.get("src/a.rs::f"), Some(&30));
    }
}
