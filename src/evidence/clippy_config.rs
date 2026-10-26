//! Freeze Clippy's configuration search, including paths outside the workspace.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

use anyhow::Result;

use crate::snapshot::digest;

pub(super) fn collect(
    root: &Path,
    inputs: &BTreeMap<String, String>,
    explicit: Option<&OsStr>,
) -> Result<BTreeMap<String, String>> {
    // Clippy's CARGO_MANIFEST_DIR fallback starts per package. Relative explicit
    // overrides use the compiler's working directory: freeze workspace and
    // package-relative searches conservatively, without outside source walks.
    let manifests: BTreeSet<_> = inputs
        .keys()
        .filter(|path| Path::new(path).file_name() == Some(OsStr::new("Cargo.toml")))
        .map(|path| Path::new(path).parent().unwrap_or(Path::new("")))
        .chain([Path::new("")])
        .collect();
    let mut controls = BTreeMap::new();
    for manifest in manifests {
        let package = root.join(manifest);
        let start = explicit.map_or_else(|| package.clone(), |path| package.join(path));
        let label = format!("@clippy/{}/", manifest.display());
        // A missing directory for a conservative search need not invalidate
        // measurement;
        // Clippy itself rejects invalid directories for the packages it runs.
        let start = start.canonicalize().unwrap_or(start);
        controls.insert(
            format!("{label}search-root"),
            digest(start.as_os_str().as_encoded_bytes()),
        );
        fingerprint_search(&start, &label, &mut controls)?;
    }
    Ok(controls)
}

fn fingerprint_search(
    start: &Path,
    label: &str,
    controls: &mut BTreeMap<String, String>,
) -> Result<()> {
    // Clippy prefers .clippy.toml, stops at the first containing directory,
    // and ignores the other name there. Freeze both names and every ancestor
    // conservatively so additions, removals and selection changes are caught.
    for (depth, directory) in start.ancestors().enumerate() {
        for name in [".clippy.toml", "clippy.toml"] {
            let candidate = directory.join(name);
            if candidate.is_file() {
                let selected = candidate.canonicalize()?;
                let key = format!("{label}{depth}/{name}");
                controls.insert(
                    format!("{key}/path"),
                    digest(selected.as_os_str().as_encoded_bytes()),
                );
                controls.insert(key, digest(&std::fs::read(selected)?));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_override_probes_freeze_workspace_and_manifest_searches() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        std::fs::create_dir_all(root.join("nested/member/config")).unwrap();
        std::fs::create_dir(root.join("config")).unwrap();
        let inputs = BTreeMap::from([("nested/member/Cargo.toml".into(), String::new())]);
        let collect = || collect(&root, &inputs, Some(OsStr::new("config"))).unwrap();
        let empty = collect();
        let path = root.join("nested/member/config/.clippy.toml");
        std::fs::write(&path, "allow-unwrap-in-tests = true").unwrap();
        let added = collect();
        assert_ne!(empty, added);
        assert!(added
            .keys()
            .any(|key| key.contains("member/0/.clippy.toml")));
        std::fs::write(&path, "allow-unwrap-in-tests = false").unwrap();
        let changed = collect();
        assert_ne!(added, changed);
        std::fs::rename(&path, path.with_file_name("clippy.toml")).unwrap();
        assert_ne!(changed, collect());
        assert_ne!(collect(), super::collect(&root, &inputs, None).unwrap());
        std::fs::remove_dir(root.join("config")).unwrap();
        assert!(!collect().is_empty());
    }
}
