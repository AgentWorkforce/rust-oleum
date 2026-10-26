//! Inventory and execution checks for measurements produced by this binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{ensure, Context, Result};
use serde_json::Value;

use crate::config::Config;
use crate::snapshot::digest;

mod clippy_config;

pub fn execute(
    root: &Path,
    program: &str,
    args: &[String],
    target: Option<&Path>,
) -> Result<String> {
    let mut command = Command::new(program);
    command.current_dir(root).args(args);
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    }
    let output = command
        .output()
        .with_context(|| format!("starting {program}"))?;
    ensure!(
        output.status.success(),
        "{program} {} failed ({})\n{}\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stdout).context("command output is not UTF-8")
}

#[derive(Debug)]
pub struct Workspace {
    pub members: BTreeSet<String>,
    pub target: PathBuf,
    pub has_library: bool,
}

pub fn workspace(root: &Path) -> Result<Workspace> {
    ensure!(
        root.join("Cargo.lock").is_file(),
        "measure requires Cargo.lock; run cargo generate-lockfile first"
    );
    let text = execute(
        root,
        "cargo",
        &strings(&["metadata", "--locked", "--format-version", "1"]),
        None,
    )?;
    let value: Value = serde_json::from_str(&text)?;
    let workspace_root = Path::new(
        value["workspace_root"]
            .as_str()
            .context("missing workspace root")?,
    )
    .canonicalize()?;
    ensure!(
        workspace_root == root,
        "run measure from the Cargo workspace root"
    );
    for package in value["packages"].as_array().context("missing packages")? {
        if package["source"].is_null() {
            let manifest = Path::new(
                package["manifest_path"]
                    .as_str()
                    .context("missing manifest path")?,
            )
            .canonicalize()?;
            ensure!(
                manifest.starts_with(root),
                "local dependency outside measured workspace: {}",
                manifest.display()
            );
        }
    }
    let members = value["workspace_members"]
        .as_array()
        .context("missing workspace members")?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).context("invalid member ID"))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(!members.is_empty(), "workspace has no members");
    let has_library = value["packages"]
        .as_array()
        .context("missing packages")?
        .iter()
        .any(|package| {
            package["id"]
                .as_str()
                .is_some_and(|id| members.contains(id))
                && package["targets"].as_array().is_some_and(|targets| {
                    targets.iter().any(|target| {
                        target["kind"].as_array().is_some_and(|kinds| {
                            kinds.iter().any(|kind| {
                                kind.as_str().is_some_and(|k| {
                                    ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"]
                                        .contains(&k)
                                })
                            })
                        })
                    })
                })
        });
    Ok(Workspace {
        members,
        has_library,
        target: PathBuf::from(
            value["target_directory"]
                .as_str()
                .context("missing target directory")?,
        ),
    })
}

pub fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

pub fn inventory(root: &Path, target: &Path) -> Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    walk(root, root, target, &mut files)?;
    Ok(files)
}

fn walk(
    root: &Path,
    dir: &Path,
    target: &Path,
    files: &mut BTreeMap<String, String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path == target
            || [
                "target",
                ".git",
                "node_modules",
                "mutants.out",
                "mutants.out.old",
                ".rust-oleum",
            ]
            .contains(&name.as_str())
        {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "symlink prevents complete source inventory: {}",
            path.display()
        );
        if kind.is_dir() {
            walk(root, &path, target, files)?;
        } else if path.extension().is_some_and(|e| e == "rs") || is_control(&path) {
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF-8 source path")?
                .replace('\\', "/");
            files.insert(relative, digest(&std::fs::read(&path)?));
        }
    }
    Ok(())
}

pub fn is_control(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    [
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain",
        "rust-toolchain.toml",
        "clippy.toml",
        ".clippy.toml",
    ]
    .contains(&name)
        || path.components().any(|c| c.as_os_str() == ".cargo")
}

pub fn controls(
    root: &Path,
    inputs: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut controls: BTreeMap<_, _> = inputs
        .iter()
        .filter(|(p, _)| is_control(Path::new(p)))
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect();
    // Cargo also reads ancestor/global configuration. Freeze their content.
    for (depth, parent) in root.ancestors().skip(1).enumerate() {
        for name in ["config", "config.toml"] {
            let path = parent.join(".cargo").join(name);
            if path.is_file() {
                controls.insert(
                    format!("@ancestor/{depth}/{name}"),
                    digest(&std::fs::read(path)?),
                );
            }
        }
    }
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    if let Some(home) = home {
        for name in ["config", "config.toml"] {
            let path = home.join(name);
            if path.is_file() {
                controls.insert(format!("@cargo-home/{name}"), digest(&std::fs::read(path)?));
            }
        }
    }
    controls.extend(clippy_config::collect(
        root,
        inputs,
        std::env::var_os("CLIPPY_CONF_DIR").as_deref(),
    )?);
    Ok(controls)
}

pub fn environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy().into_owned();
            (key.starts_with("RUST")
                || key.starts_with("CARGO")
                || ["CLIPPY_CONF_DIR", "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS"]
                    .contains(&key.as_str()))
            .then(|| (key, digest(value.as_encoded_bytes())))
        })
        .collect()
}

pub fn validate_roots(root: &Path, config: &Config) -> Result<()> {
    ensure!(
        !config.sources.rust_roots.is_empty(),
        "no source roots configured"
    );
    for source in &config.sources.rust_roots {
        let path = root
            .join(source)
            .canonicalize()
            .with_context(|| format!("source root does not exist: {source}"))?;
        ensure!(
            path.starts_with(root) && path.is_dir(),
            "invalid source root {source}"
        );
    }
    Ok(())
}

/// Strictly validate completion, diagnostics and fresh workspace-member coverage.
/// Return only workspace diagnostics so dependencies cannot pollute the score.
pub fn validate_clippy(text: &str, members: &BTreeSet<String>) -> Result<String> {
    let mut rebuilt = BTreeSet::new();
    let mut finished = false;
    let mut log = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        ensure!(!finished, "Clippy records after build-finished");
        let value: Value = serde_json::from_str(line).context("malformed Clippy JSON")?;
        ensure!(
            value.pointer("/message/level").and_then(Value::as_str) != Some("error"),
            "Clippy reported a compiler error"
        );
        ensure!(
            !matches!(
                value.pointer("/message/code/code").and_then(Value::as_str),
                Some("unknown_lints" | "renamed_and_removed_lints")
            ),
            "Clippy reported an unknown or removed lint"
        );
        let local = value["package_id"]
            .as_str()
            .is_some_and(|id| members.contains(id));
        match value["reason"].as_str() {
            Some("build-finished") => {
                ensure!(value["success"] == true, "Clippy build failed");
                finished = true;
            }
            Some("compiler-artifact") if local && value["fresh"] == false => {
                rebuilt.insert(
                    value["package_id"]
                        .as_str()
                        .context("missing package ID")?
                        .to_string(),
                );
            }
            _ => {}
        }
        if local {
            log.push_str(line);
            log.push('\n');
        }
    }
    ensure!(
        finished,
        "Clippy log has no successful build-finished record"
    );
    ensure!(
        members.is_subset(&rebuilt),
        "Clippy did not rebuild every workspace member"
    );
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(package: &str, fresh: bool) -> String {
        serde_json::json!({"reason":"compiler-artifact", "package_id":package, "fresh":fresh})
            .to_string()
            + "\n"
    }

    #[test]
    fn clippy_requires_completed_fresh_runs_for_every_member() {
        let members = BTreeSet::from(["one".into(), "two".into()]);
        let done = "{\"reason\":\"build-finished\",\"success\":true}\n";
        let rebuilt = artifact("one", false) + &artifact("two", false);
        let complete = rebuilt.clone() + done;
        assert_eq!(validate_clippy(&complete, &members).unwrap(), rebuilt);
        assert_eq!(
            validate_clippy(&(artifact("dependency", false) + &complete), &members).unwrap(),
            rebuilt
        );
        for invalid in [
            String::new(), rebuilt.clone(), done.to_string(),
            artifact("one", false) + &artifact("two", true) + done,
            complete.clone() + "{}\n", "{broken}\n".into(),
            rebuilt.clone() + "{\"reason\":\"build-finished\",\"success\":false}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\"}}\n".to_string() + &complete,
            "{\"reason\":\"compiler-message\",\"message\":{\"code\":{\"code\":\"unknown_lints\"}}}\n".to_string() + &complete,
        ] {
            assert!(validate_clippy(&invalid, &members).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn inventory_tracks_sources_and_controls_and_ignores_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        for name in ["src", "target", ".cargo", "custom-target", ".rust-oleum"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        for (name, data) in [
            ("src/lib.rs", "pub fn f() {}"),
            ("Cargo.toml", "manifest"),
            ("build.rs", "fn main() {}"),
            (".cargo/config.toml", "[build]"),
            ("target/ignored.rs", ""),
            ("custom-target/ignored.rs", ""),
            (".rust-oleum/ignored.rs", ""),
            ("notes.txt", "not source"),
        ] {
            std::fs::write(root.join(name), data).unwrap();
        }
        let files = inventory(&root, &root.join("custom-target")).unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!(files["src/lib.rs"], digest(b"pub fn f() {}"));
        std::fs::create_dir(dir.path().join(".cargo")).unwrap();
        std::fs::write(dir.path().join(".cargo/config.toml"), "[build]").unwrap();
        let control = controls(&root, &files).unwrap();
        assert!(control.contains_key("@ancestor/0/config.toml"));
        assert!(control.contains_key("build.rs"));
        assert!(!control.contains_key("src/lib.rs"));
        assert!(environment().values().all(|h| h.len() == 64));
        let valid = Config::parse("[targets]\n[sources]\nrust_roots=[\"src\"]").unwrap();
        validate_roots(&root.canonicalize().unwrap(), &valid).unwrap();
        for roots in ["[]", "[\"absent\"]", "[\"../\"]", "[\"Cargo.toml\"]"] {
            let config =
                Config::parse(&format!("[targets]\n[sources]\nrust_roots={roots}")).unwrap();
            assert!(validate_roots(&root.canonicalize().unwrap(), &config).is_err());
        }
        assert!(workspace(&root)
            .unwrap_err()
            .to_string()
            .contains("Cargo.lock"));
        assert!(execute(&root, "oleum-nonexistent-command", &[], None).is_err());
        assert!(execute(&root, "cargo", &strings(&["--not-a-cargo-option"]), None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_explicitly_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", dir.path().join("linked.rs")).unwrap();
        assert!(inventory(dir.path(), &dir.path().join("target")).is_err());
    }
}
