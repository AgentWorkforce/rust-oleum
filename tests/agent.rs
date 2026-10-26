//! Real Cargo integration tests for the agent protocol. No mocked compiler logs.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

struct Project(tempfile::TempDir);

const SOURCE: &str = "pub fn choose(x: Option<u8>) -> u8 { x.unwrap() }\n#[cfg(test)] mod tests { #[test] fn behavior() { assert_eq!(super::choose(Some(7)), 7); } }\n";
const POLICY: &str = "[targets]\n[sources]\nrust_roots=[\"src\"]\n[lint_groups.panics]\nlints=[\"clippy::unwrap_used\"]\n[baseline.panic]\n\"src/lib.rs::choose\"=1\n[baseline.lint_groups.panics]\n\"clippy::unwrap_used@src/lib.rs::choose\"=1\n";

impl Project {
    fn new() -> Self {
        let project = Self(tempfile::tempdir().unwrap());
        std::fs::create_dir(project.root().join("src")).unwrap();
        project.write(
            "Cargo.toml",
            "[package]\nname=\"agent_fixture\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        );
        project.write("src/lib.rs", SOURCE);
        project.write("rust-oleum.toml", POLICY);
        let output = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(project.root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        project
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.root().join(file), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rust-oleum"))
            .args(args)
            .current_dir(self.root())
            .output()
            .unwrap()
    }

    fn measure(&self, name: &str) -> Value {
        let out = self.run(&["measure", "--output", name]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&std::fs::read(self.root().join(name)).unwrap()).unwrap()
    }

    fn compare(&self, before: &str, after: &str, code: i32) -> Value {
        let out = self.run(&["compare", "--before", before, "--after", after]);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

#[test]
fn real_measurements_repeat_and_reward_debt_reduction_before_tightening() {
    let project = Project::new();
    let before = project.measure("before.json");
    assert_eq!(before, project.measure("repeat.json"));
    assert_eq!(
        project.compare("before.json", "repeat.json", 1)["quality_change"],
        "unchanged"
    );
    assert_eq!(before["checks"]["coverage"], "not_measured");
    assert_eq!(before["checks"]["tests"], "passed");
    assert!(before["context"]["enabled_lints"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l == "clippy::unwrap_used"));
    assert_eq!(
        before["observations"]["lints.panics@clippy::unwrap_used@src/lib.rs::choose"]["value"],
        1
    );
    assert!(before["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["code"] == "clippy::unwrap_used" && f["line"] == 1));
    project.write(
        "src/lib.rs",
        &SOURCE.replace("x.unwrap()", "x.unwrap_or_default()"),
    );
    project.measure("after.json");
    let comparison = project.compare("before.json", "after.json", 0);
    assert_eq!(comparison["quality_change"], "improved");
    assert_eq!(comparison["baseline_action"], "tighten_required");
    assert_eq!(comparison["eligible_to_tighten"], true);
    assert!(comparison["regressions"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(project.root().join("rust-oleum.toml")).unwrap(),
        POLICY
    );
    let prior = std::fs::read(project.root().join("before.json")).unwrap();
    assert!(!project
        .run(&["measure", "--output", "before.json"])
        .status
        .success());
    assert_eq!(
        std::fs::read(project.root().join("before.json")).unwrap(),
        prior
    );
    assert_eq!(
        project.compare("after.json", "before.json", 1)["quality_change"],
        "regressed"
    );
}

#[test]
fn policy_and_existing_test_changes_require_review() {
    let project = Project::new();
    project.measure("before.json");
    project.write(
        "rust-oleum.toml",
        &POLICY.replace("[targets]", "[targets]\npanic_max=3"),
    );
    project.measure("policy.json");
    assert_eq!(
        project.compare("before.json", "policy.json", 2)["quality_change"],
        "incomparable"
    );
    project.write("rust-oleum.toml", POLICY);
    project.write(
        "src/lib.rs",
        &SOURCE.replace("assert_eq!(super::choose(Some(7)), 7);", ""),
    );
    project.measure("test.json");
    let comparison = project.compare("before.json", "test.json", 2);
    assert!(comparison["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r.as_str().unwrap().contains("existing test")));
}

#[test]
fn failures_never_publish_a_success_snapshot() {
    let project = Project::new();
    for source in ["not Rust", &SOURCE.replace("Some(7)), 7", "Some(7)), 8")] {
        project.write("src/lib.rs", source);
        let out = project.run(&["measure", "--output", "bad.json"]);
        assert!(!out.status.success());
        assert!(!project.root().join("bad.json").exists());
    }
    project.write("src/lib.rs", SOURCE);
    for policy in [
        POLICY.replace("clippy::unwrap_used", "clippy::unknown_rule"),
        POLICY.replace("[\"src\"]", "[\"missing\"]"),
    ] {
        project.write("rust-oleum.toml", &policy);
        assert!(!project
            .run(&["measure", "--output", "bad.json"])
            .status
            .success());
        assert!(!project.root().join("bad.json").exists());
    }
    project.write("rust-oleum.toml", POLICY);
    let mut snapshot = project.measure("valid.json");
    snapshot["policy"]["targets"]["panic_max"] = 100.into();
    project.write("tampered.json", &snapshot.to_string());
    let out = project.run(&[
        "compare",
        "--before",
        "valid.json",
        "--after",
        "tampered.json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("integrity"));
}

#[test]
fn wildcard_lints_and_workspace_members_are_measured_even_with_suppressions() {
    let project = Project::new();
    project.write(
        "Cargo.toml",
        "[workspace]\nmembers=[\"one\",\"two\"]\nresolver=\"2\"\n",
    );
    for name in ["one", "two"] {
        std::fs::create_dir_all(project.root().join(name).join("src")).unwrap();
        project.write(
            &format!("{name}/Cargo.toml"),
            &format!("[package]\nname=\"{name}\"\nversion=\"0.0.0\"\nedition=\"2021\"\n"),
        );
        project.write(
            &format!("{name}/src/lib.rs"),
            "#[allow(clippy::unwrap_used)] pub fn f(x: Option<u8>) -> u8 { x.unwrap() }\n",
        );
    }
    project.write("rust-oleum.toml", "[targets]\n[sources]\nrust_roots=[\"one/src\",\"two/src\"]\n[lint_groups.panics]\nlints=[\"clippy::unwrap_*\"]\n");
    assert!(Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(project.root())
        .status()
        .unwrap()
        .success());
    let snapshot = project.measure("workspace.json");
    for name in ["one", "two"] {
        let key = format!("lints.panics@clippy::unwrap_used@{name}/src/lib.rs::f");
        assert_eq!(snapshot["observations"][key]["value"], 1);
    }
}

#[test]
fn binary_only_workspace_records_doctests_as_not_applicable() {
    let project = Project::new();
    std::fs::remove_file(project.root().join("src/lib.rs")).unwrap();
    project.write("src/main.rs", "fn main() {}\n");
    let snapshot = project.measure("binary.json");
    assert_eq!(snapshot["context"]["doctests"], false);
    assert_eq!(snapshot["checks"]["doctests"], "not_applicable");
    assert_eq!(
        project.compare("binary.json", "binary.json", 1)["quality_change"],
        "unchanged"
    );
}

#[test]
fn measurement_requires_a_lockfile_without_creating_one() {
    let project = Project::new();
    let lockfile = project.root().join("Cargo.lock");
    std::fs::remove_file(&lockfile).unwrap();
    let output = project.run(&["measure", "--output", "unlocked.json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("measure requires Cargo.lock"));
    assert!(!lockfile.exists());
    assert!(!project.root().join("unlocked.json").exists());
}

#[test]
fn measurement_rejects_local_dependencies_outside_its_inventory() {
    let project = Project::new();
    let dependency = tempfile::tempdir().unwrap();
    std::fs::create_dir(dependency.path().join("src")).unwrap();
    std::fs::write(
        dependency.path().join("Cargo.toml"),
        "[package]\nname=\"outside_fixture\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(dependency.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    project.write(
        "Cargo.toml",
        &format!(
            "[package]\nname=\"agent_fixture\"\nversion=\"0.0.0\"\nedition=\"2021\"\n\
             [dependencies]\noutside_fixture={{path={:?}}}\n",
            dependency.path()
        ),
    );
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(project.root())
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let original_lock = std::fs::read(project.root().join("Cargo.lock")).unwrap();
    let output = project.run(&["measure", "--output", "outside.json"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("local dependency outside measured workspace"));
    assert!(!project.root().join("outside.json").exists());
    assert_eq!(
        std::fs::read(project.root().join("Cargo.lock")).unwrap(),
        original_lock
    );
}

#[test]
fn comparison_output_preserves_existing_reports_and_snapshots() {
    let project = Project::new();
    project.measure("before.json");
    let output = project.run(&[
        "compare",
        "--before",
        "before.json",
        "--after",
        "before.json",
        "--output",
        "comparison.json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let report: Value =
        serde_json::from_slice(&std::fs::read(project.root().join("comparison.json")).unwrap())
            .unwrap();
    assert_eq!(report["quality_change"], "unchanged");
    assert_eq!(report["measurement"], "complete");
    assert_eq!(report["eligible_to_tighten"], false);
    for destination in ["comparison.json", "before.json"] {
        let original = std::fs::read(project.root().join(destination)).unwrap();
        let rejected = project.run(&[
            "compare",
            "--before",
            "before.json",
            "--after",
            "before.json",
            "--output",
            destination,
        ]);
        assert_eq!(rejected.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("output must not already exist"));
        assert_eq!(
            std::fs::read(project.root().join(destination)).unwrap(),
            original
        );
    }
}

#[test]
fn global_cargo_configuration_changes_make_measurements_incomparable() {
    let project = Project::new();
    let cargo_home = tempfile::tempdir().unwrap();
    let mut snapshots = Vec::new();
    for (name, jobs) in [("one-job.json", 1), ("two-jobs.json", 2)] {
        std::fs::write(
            cargo_home.path().join("config.toml"),
            format!("[build]\njobs = {jobs}\n"),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rust-oleum"))
            .args(["measure", "--output", name])
            .env("CARGO_HOME", cargo_home.path())
            .env("CARGO_NET_OFFLINE", "true")
            .current_dir(project.root())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(project.root().join(name)).unwrap()).unwrap();
        assert_eq!(
            snapshot["context"]["controls"]["@cargo-home/config.toml"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        snapshots.push(snapshot);
    }
    let before = &snapshots[0];
    let after = &snapshots[1];
    assert_eq!(before["inputs"], after["inputs"]);
    assert_eq!(before["policy"], after["policy"]);
    assert_eq!(before["observations"], after["observations"]);
    assert_eq!(
        before["context"]["environment"],
        after["context"]["environment"]
    );
    assert_ne!(
        before["context"]["controls"]["@cargo-home/config.toml"],
        after["context"]["controls"]["@cargo-home/config.toml"]
    );
    let comparison = project.compare("one-job.json", "two-jobs.json", 2);
    assert_eq!(comparison["quality_change"], "incomparable");
    assert_eq!(comparison["eligible_to_tighten"], false);
    assert!(comparison["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reason| reason
            .as_str()
            .unwrap()
            .contains("measurement context changed")));
}

#[test]
fn coverage_measurements_reward_added_tests_and_reject_failing_tests() {
    // Coverage is optional for local development. CI installs cargo-llvm-cov
    // before its coverage phase, which reruns this test with the tool available.
    let available = Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !available {
        eprintln!(
            "skipping coverage integration test: install cargo-llvm-cov and llvm-tools-preview"
        );
        return;
    }

    let project = Project::new();
    let source = "pub fn covered() -> u8 {\n    7\n}\n\
                  pub fn uncovered() -> u8 {\n    9\n}\n\
                  #[cfg(test)] mod tests {\n\
                  #[test] fn original() { assert_eq!(super::covered(), 7); }\n}\n";
    project.write("src/lib.rs", source);
    let policy = "[targets]\n[sources]\nrust_roots=[\"src\"]\n[baseline]\ncoverage_min_pct=0.0\n";
    project.write("rust-oleum.toml", policy);
    let mut snapshots = Vec::new();
    for (name, extra_test) in [
        ("coverage-before.json", ""),
        (
            "coverage-after.json",
            "\n#[test] fn additional() { assert_eq!(uncovered(), 9); }\n",
        ),
    ] {
        project.write("src/lib.rs", &format!("{source}{extra_test}"));
        let output = run_coverage_fixture(&project, name);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: Value =
            serde_json::from_slice(&std::fs::read(project.root().join(name)).unwrap()).unwrap();
        assert_eq!(snapshot["context"]["coverage"], true);
        assert!(snapshot["context"]["llvm_cov"]
            .as_str()
            .unwrap()
            .contains("cargo-llvm-cov"));
        assert_eq!(snapshot["checks"]["tests"], "passed");
        assert_eq!(snapshot["checks"]["coverage"], "passed");
        assert_eq!(snapshot["checks"]["doctests"], "passed");
        snapshots.push(snapshot);
    }
    let coverage = "coverage@@project";
    let crap = "crap@src/lib.rs::uncovered";
    assert!(
        snapshots[0]["observations"][coverage]["value"]
            .as_u64()
            .unwrap()
            < 10000
    );
    assert_eq!(snapshots[1]["observations"][coverage]["value"], 10000);
    assert!(
        snapshots[0]["observations"][crap]["value"]
            .as_u64()
            .unwrap()
            > 10
    );
    assert_eq!(snapshots[1]["observations"][crap]["value"], 10);
    let comparison = project.compare("coverage-before.json", "coverage-after.json", 0);
    assert_eq!(comparison["quality_change"], "improved");
    assert!(comparison["regressions"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(project.root().join("rust-oleum.toml")).unwrap(),
        policy
    );

    project.write(
        "src/lib.rs",
        &format!("{source}\n#[test] fn fails() {{ assert_eq!(uncovered(), 0); }}\n"),
    );
    let failed = run_coverage_fixture(&project, "failed.json");
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("test result: FAILED"));
    assert!(!project.root().join("failed.json").exists());
}

fn run_coverage_fixture(project: &Project, output: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rust-oleum"));
    command
        .args(["measure", "--coverage", "--output", output])
        .current_dir(project.root());
    // The fixture owns its coverage session. Inheriting the outer session's
    // compiler wrapper makes cargo-llvm-cov wrap itself recursively. Preserve
    // LLVM_PROFILE_FILE so the already-instrumented rust-oleum binary still
    // reports its own execution to the outer coverage run.
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("__CARGO_LLVM_COV")
            || name.starts_with("CARGO_LLVM_COV")
            || [
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "RUSTFLAGS",
                "CARGO_ENCODED_RUSTFLAGS",
            ]
            .contains(&name.as_ref())
        {
            command.env_remove(key);
        }
    }
    // Tests must not prompt for or install toolchain components.
    command.env("CARGO_LLVM_COV_SETUP", "no");
    command.output().unwrap()
}
