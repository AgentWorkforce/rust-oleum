//! Regression tests for external Clippy configuration in measurement evidence.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

struct Project {
    temp: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        let project = Self {
            temp: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(project.root().join("nested/member/src")).unwrap();
        project.write(
            "Cargo.toml",
            "[workspace]\nmembers=[\"nested/member\"]\nresolver=\"2\"\n",
        );
        project.write(
            "nested/member/Cargo.toml",
            "[package]\nname=\"clippy_review\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        );
        project.write("nested/member/src/lib.rs", "pub fn identity(value: u8) -> u8 { value }\n#[cfg(test)] mod tests { #[test] fn behavior() { let value = Some(super::identity(7)); assert_eq!(value.unwrap(), 7); } }\n");
        project.write("rust-oleum.toml", "[targets]\n[sources]\nrust_roots=[\"nested/member/src\"]\n[lint_groups.panics]\nlints=[\"clippy::unwrap_used\"]\n");
        let output = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(project.root())
            .output()
            .unwrap();
        assert_success(&output);
        project
    }

    fn root(&self) -> std::path::PathBuf {
        self.temp.path().join("workspace")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.root().join(name), text).unwrap();
    }

    fn run(&self, args: &[&str], config: Option<&Path>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rust-oleum"));
        command
            .current_dir(self.root())
            .args(args)
            .env_remove("CLIPPY_CONF_DIR");
        if let Some(config) = config {
            command.env("CLIPPY_CONF_DIR", config);
        }
        command.output().unwrap()
    }

    fn measure(&self, name: &str, config: Option<&Path>) -> Value {
        assert_success(&self.run(&["measure", "--output", name], config));
        serde_json::from_slice(&std::fs::read(self.root().join(name)).unwrap()).unwrap()
    }

    fn incomparable(&self, before: &str, after: &str) {
        let output = self.run(&["compare", "--before", before, "--after", after], None);
        assert_eq!(output.status.code(), Some(2));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["quality_change"], "incomparable");
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn unwrap_count(snapshot: &Value) -> usize {
    snapshot["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["code"] == "clippy::unwrap_used")
        .count()
}

#[test]
fn ancestor_config_additions_and_changes_cannot_claim_improvement() {
    let project = Project::new();
    let before = project.measure("before.json", None);
    assert_eq!(unwrap_count(&before), 1);
    let config = project.temp.path().join(".clippy.toml");
    std::fs::write(&config, "allow-unwrap-in-tests = false\n").unwrap();
    project.measure("added.json", None);
    project.incomparable("before.json", "added.json");
    std::fs::write(&config, "allow-unwrap-in-tests = true\n").unwrap();
    let after = project.measure("after.json", None);
    assert_eq!(unwrap_count(&after), 0);
    assert_ne!(before["context"]["controls"], after["context"]["controls"]);
    project.incomparable("added.json", "after.json");
}

#[test]
fn explicit_config_contents_and_selection_are_frozen() {
    let project = Project::new();
    let config = project.temp.path().join("external");
    std::fs::create_dir_all(config.join("child")).unwrap();
    let selected = config.join("child");
    std::fs::write(
        config.join("clippy.toml"),
        "allow-unwrap-in-tests = false\n",
    )
    .unwrap();
    let before = project.measure("before.json", Some(&selected));
    assert_eq!(unwrap_count(&before), 1);
    assert!(before["context"]["environment"]["CLIPPY_CONF_DIR"].is_string());
    std::fs::write(config.join("clippy.toml"), "allow-unwrap-in-tests = true\n").unwrap();
    let after = project.measure("after.json", Some(&selected));
    assert_eq!(unwrap_count(&after), 0);
    project.incomparable("before.json", "after.json");
    std::fs::write(
        selected.join(".clippy.toml"),
        "allow-unwrap-in-tests = true\n",
    )
    .unwrap();
    project.measure("shadowed.json", Some(&selected));
    project.incomparable("after.json", "shadowed.json");
    project.measure("selected.json", Some(&config));
    project.incomparable("after.json", "selected.json");
    project.measure("unset.json", None);
    project.incomparable("selected.json", "unset.json");
}

#[test]
fn relative_override_freezes_workspace_and_nested_package_searches() {
    let project = Project::new();
    let config = project.root().join("nested/config");
    std::fs::create_dir(&config).unwrap();
    // The compiler's relative override starts at the workspace root. Also
    // freeze package-relative locations conservatively for nested workspaces.
    std::fs::create_dir(project.temp.path().join("config")).unwrap();
    std::fs::write(
        project.temp.path().join("config/clippy.toml"),
        "allow-unwrap-in-tests = false\n",
    )
    .unwrap();
    std::fs::write(
        config.join("clippy.toml"),
        "allow-unwrap-in-tests = false\n",
    )
    .unwrap();
    let relative = Path::new("../config");
    let before = project.measure("before.json", Some(relative));
    assert_eq!(unwrap_count(&before), 1);
    std::fs::write(config.join("clippy.toml"), "allow-unwrap-in-tests = true\n").unwrap();
    let nested = project.measure("nested.json", Some(relative));
    assert_eq!(unwrap_count(&nested), 1);
    project.incomparable("before.json", "nested.json");
    std::fs::write(
        project.temp.path().join("config/clippy.toml"),
        "allow-unwrap-in-tests = true\n",
    )
    .unwrap();
    let after = project.measure("after.json", Some(relative));
    assert_eq!(unwrap_count(&after), 0);
    project.incomparable("before.json", "after.json");
}

#[test]
fn config_created_during_execution_invalidates_measurement() {
    let project = Project::new();
    let config = project.temp.path().join("clippy.toml");
    project.write(
        "nested/member/build.rs",
        &format!(
            "fn main() {{ std::fs::write({:?}, \"allow-unwrap-in-tests = true\\n\").unwrap(); }}\n",
            config
        ),
    );
    let output = project.run(&["measure", "--output", "unstable.json"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("context changed during execution"));
    assert!(!project.root().join("unstable.json").exists());
}
