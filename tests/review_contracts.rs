//! Cargo regressions for module reachability and intentional invalid fixtures.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const LIB: &str = "pub mod debt; pub fn stable() -> u8 { 7 }";
const DEBT: &str = "pub fn choose(x: Option<u8>) -> u8 { x.unwrap() }";

struct Project(tempfile::TempDir);

impl Project {
    fn new() -> Self {
        let project = Self(tempfile::tempdir().unwrap());
        project.write(
            "Cargo.toml",
            "[package]\nname=\"contracts_fixture\"\nversion=\"0.0.0\"\nedition=\"2021\"\n",
        );
        project.write("src/lib.rs", LIB);
        project.write("src/debt.rs", DEBT);
        project.write("rust-oleum.toml", "[targets]\n[sources]\nrust_roots=[\"src\"]\n[lint_groups.panics]\nlints=[\"clippy::unwrap_used\"]\n");
        let output = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(project.root())
            .output()
            .unwrap();
        assert!(output.status.success());
        project
    }

    fn root(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, source: &str) {
        let path = self.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
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

    fn incomparable(&self, after: &str, reason: &str) {
        let out = self.run(&["compare", "--before", "before.json", "--after", after]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(report["quality_change"], "incomparable");
        assert!(report["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r.as_str().unwrap().contains(reason)));
    }
}

#[test]
fn disconnecting_production_or_test_modules_requires_review() {
    let project = Project::new();
    project.write("tests/integration.rs", "mod cases;");
    project.write(
        "tests/cases/mod.rs",
        "#[test] fn behavior() { assert_eq!(contracts_fixture::stable(), 7); }",
    );
    let before = project.measure("before.json");
    project.write("src/lib.rs", "pub fn stable() -> u8 { 7 }");
    let after = project.measure("production.json");
    assert_eq!(
        before["inputs"]["src/debt.rs"],
        after["inputs"]["src/debt.rs"]
    );
    assert!(!before["findings"].as_array().unwrap().is_empty());
    assert!(after["findings"].as_array().unwrap().is_empty());
    project.incomparable("production.json", "src/lib.rs::debt");

    project.write("src/lib.rs", LIB);
    project.write(
        "src/debt.rs",
        &DEBT.replace("x.unwrap()", "x.unwrap_or_default()"),
    );
    project.write("tests/integration.rs", "");
    let after = project.measure("tests.json");
    assert_eq!(before["test_contracts"], after["test_contracts"]);
    project.incomparable("tests.json", "tests/integration.rs::cases");

    project.write(
        "tests/integration.rs",
        "#[path = \"alternate/mod.rs\"] mod cases;",
    );
    project.write("tests/alternate/mod.rs", "#[test] fn behavior() {}");
    project.measure("redirected.json");
    project.incomparable("redirected.json", "tests/integration.rs::cases");
}

#[test]
fn invalid_ui_fixtures_are_preserved_and_production_syntax_still_fails() {
    let project = Project::new();
    project.write(
        "tests/ui/invalid.rs",
        "fn invalid( { intentional syntax error",
    );
    let before = project.measure("before.json");
    assert!(before["inputs"]["tests/ui/invalid.rs"].is_string());
    project.write(
        "src/debt.rs",
        &DEBT.replace("x.unwrap()", "x.unwrap_or_default()"),
    );
    project.measure("improved.json");
    let out = project.run(&[
        "compare",
        "--before",
        "before.json",
        "--after",
        "improved.json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    project.write(
        "tests/ui/invalid.rs",
        "fn changed( { intentional syntax error",
    );
    project.measure("fixture.json");
    project.incomparable("fixture.json", "unparsed-fixture");
    project.write("src/lib.rs", "fn broken( {");
    let out = project.run(&["measure", "--output", "broken.json"]);
    assert!(!out.status.success());
    assert!(!project.root().join("broken.json").exists());
}
