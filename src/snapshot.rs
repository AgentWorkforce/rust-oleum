//! Versioned, canonical evidence for an agent's edit/measure/compare loop.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

pub const SCHEMA_VERSION: u32 = 1;

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextInfo {
    pub analyzer_version: String,
    pub analyzer_sha256: String,
    pub rustc: String,
    pub cargo: String,
    pub clippy: String,
    pub llvm_cov: Option<String>,
    pub enabled_lints: Vec<String>,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub coverage: bool,
    pub doctests: bool,
    /// Hashes, never raw potentially secret environment values.
    pub environment: BTreeMap<String, String>,
    /// Manifests, lockfile, toolchain and lint/build configuration.
    pub controls: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Lower,
    Higher,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub metric: String,
    pub item: String,
    /// File or fully qualified function owning the observation.
    pub entity: String,
    pub file: String,
    pub line: Option<usize>,
    /// Fixed-point values: divide by scale to display. None means unmeasured.
    pub value: Option<u64>,
    pub target: u64,
    pub scale: u64,
    pub direction: Direction,
}

impl Observation {
    pub fn key(&self) -> String {
        format!("{}@{}", self.metric, self.item)
    }

    pub fn excess(&self) -> Option<u64> {
        self.value.map(|value| match self.direction {
            Direction::Lower => value.saturating_sub(self.target),
            Direction::Higher => self.target.saturating_sub(value),
        })
    }

    pub fn is_lint(&self) -> bool {
        self.metric == "dead_code"
            || self.metric == "redundant_code"
            || self.metric.starts_with("lints.")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    /// Detect accidental edits; not an authentication signature.
    pub snapshot_sha256: String,
    pub context: ContextInfo,
    pub policy: Config,
    pub inputs: BTreeMap<String, String>,
    pub test_contracts: BTreeMap<String, String>,
    pub compilation_gates: BTreeMap<String, String>,
    pub entities: BTreeSet<String>,
    pub checks: BTreeMap<String, String>,
    pub observations: BTreeMap<String, Observation>,
    pub findings: Vec<crate::external::Finding>,
    pub baseline_needs_tightening: bool,
}

impl Snapshot {
    pub fn seal(&mut self) -> Result<()> {
        self.snapshot_sha256.clear();
        self.snapshot_sha256 = digest(&serde_json::to_vec(self)?);
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let snapshot: Self = serde_json::from_slice(&bytes).context("parsing snapshot")?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "unsupported snapshot schema"
        );
        self.policy.validate()?;
        let mut expected = self.clone();
        expected.seal()?;
        ensure!(
            expected.snapshot_sha256 == self.snapshot_sha256,
            "snapshot integrity check failed"
        );
        ensure!(
            !self.inputs.is_empty() && !self.entities.is_empty() && !self.observations.is_empty(),
            "empty source inventory"
        );
        for (key, observation) in &self.observations {
            ensure!(*key == observation.key(), "invalid observation ID {key}");
            ensure!(
                [1, 10, 100].contains(&observation.scale),
                "invalid scale for {key}"
            );
            ensure!(
                self.entities.contains(&observation.entity),
                "missing entity for {key}"
            );
        }
        Ok(())
    }
}

/// Publish only complete results; never overwrite a starting snapshot or source file.
pub fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path).with_context(|| {
        format!(
            "publishing {}; output must not already exist",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn fixture() -> Snapshot {
    let policy = Config::parse("[targets]\n[sources]\nrust_roots=[\"src\"]\n").unwrap();
    let observation = Observation {
        metric: "cognitive".into(),
        item: "src/lib.rs::f".into(),
        entity: "src/lib.rs::f".into(),
        file: "src/lib.rs".into(),
        line: Some(1),
        value: Some(37),
        target: 22,
        scale: 1,
        direction: Direction::Lower,
    };
    let mut snapshot = Snapshot {
        schema_version: SCHEMA_VERSION,
        snapshot_sha256: String::new(),
        policy,
        context: ContextInfo {
            analyzer_version: "test".into(),
            analyzer_sha256: "test".into(),
            rustc: "test".into(),
            cargo: "test".into(),
            clippy: "test".into(),
            llvm_cov: None,
            enabled_lints: vec!["dead_code".into()],
            features: vec![],
            all_features: false,
            no_default_features: false,
            coverage: false,
            doctests: true,
            environment: BTreeMap::new(),
            controls: BTreeMap::new(),
        },
        inputs: BTreeMap::from([("src/lib.rs".into(), digest(b"source"))]),
        test_contracts: BTreeMap::new(),
        compilation_gates: BTreeMap::new(),
        entities: BTreeSet::from(["src/lib.rs".into(), "src/lib.rs::f".into()]),
        checks: ["source", "clippy", "tests", "doctests"]
            .into_iter()
            .map(|name| (name.into(), "passed".into()))
            .collect(),
        observations: BTreeMap::from([(observation.key(), observation)]),
        findings: Vec::new(),
        baseline_needs_tightening: false,
    };
    snapshot.seal().unwrap();
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trip_preserves_integrity_and_never_overwrites() {
        assert_eq!(
            digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let snapshot = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write_new(&path, &snapshot).unwrap();
        assert_eq!(Snapshot::load(&path).unwrap(), snapshot);
        assert!(write_new(&path, &serde_json::json!({})).is_err());
        assert_eq!(Snapshot::load(&path).unwrap(), snapshot);
        let mut changed = snapshot.clone();
        changed.policy.targets.panic_max = 10;
        assert!(changed
            .validate()
            .unwrap_err()
            .to_string()
            .contains("integrity"));
        assert!(Snapshot::load(&dir.path().join("absent")).is_err());
        std::fs::write(&path, "not JSON").unwrap();
        assert!(Snapshot::load(&path).is_err());
    }

    #[test]
    fn snapshots_reject_unsupported_or_inconsistent_contracts() {
        let base = fixture();
        let mut cases = Vec::new();
        let mut s = base.clone();
        s.schema_version += 1;
        cases.push(s);
        let mut s = base.clone();
        s.inputs.clear();
        cases.push(s);
        let mut s = base.clone();
        s.entities.clear();
        cases.push(s);
        let mut s = base.clone();
        s.observations.clear();
        cases.push(s);
        let key = "cognitive@src/lib.rs::f";
        let mut s = base.clone();
        s.observations.get_mut(key).unwrap().scale = 0;
        cases.push(s);
        let mut s = base.clone();
        s.observations.get_mut(key).unwrap().item = "other".into();
        cases.push(s);
        let mut s = base.clone();
        s.observations.get_mut(key).unwrap().entity = "absent".into();
        cases.push(s);
        let mut s = base;
        s.policy.targets.coverage_min_pct = 101.0;
        cases.push(s);
        for mut s in cases {
            s.seal().unwrap();
            assert!(s.validate().is_err());
        }
    }
}
