//! End-to-end tests that run the real `rust-oleum` binary against a scratch
//! crate: `init`, the gate's exit codes and outputs, every optional input,
//! `--write-baseline`, and `--tighten`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A throwaway crate with one tidy function and one over-complex one.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let w = |rel: &str, content: &str| {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };
        w(
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
        );
        w(
            "src/lib.rs",
            "pub fn tidy(x: u8) -> u8 { x + 1 }\n\
             pub fn knotty(a: bool, b: bool, c: bool) -> u8 {\n\
             \x20   if a { if b { if c { 1 } else { 2 } } else { 3 } } else { 4 }\n\
             }\n",
        );
        Self { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.root().join(rel);
        std::fs::write(&p, content).unwrap();
        p
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root().join(rel)).unwrap()
    }

    /// Targets strict enough that `knotty` violates cyclomatic/cognitive.
    fn strict_config(&self) {
        self.write(
            "rust-oleum.toml",
            "[targets]\ncyclomatic_max = 2\ncognitive_max = 2\n\n[sources]\nrust_roots = [\"src\"]\n",
        );
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_rust-oleum"))
            .current_dir(self.root())
            .args(args)
            .output()
            .expect("spawn rust-oleum")
    }

    /// lcov covering `tidy` (line 1) fully and `knotty` (lines 2-4) not at all.
    fn lcov(&self) -> PathBuf {
        self.write(
            "lcov.info",
            "SF:src/lib.rs\nDA:1,3\nDA:2,0\nDA:3,0\nDA:4,0\nend_of_record\n",
        )
    }

    /// A clippy JSON log from a rebuilt local crate with one dead-code
    /// finding inside `tidy`.
    fn clippy_log(&self) -> PathBuf {
        self.write(
            "clippy.json",
            concat!(
                r#"{"reason":"compiler-artifact","package_id":"path+file:///fx#0.1.0","fresh":false}"#,
                "\n",
                r#"{"reason":"compiler-message","message":{"code":{"code":"unused_variables"},"message":"unused variable: `y`","spans":[{"file_name":"src/lib.rs","line_start":1,"column_start":5,"is_primary":true}]}}"#,
                "\n",
                r#"{"reason":"build-finished","success":true}"#,
                "\n",
            ),
        )
    }

    fn mutants(&self, missed: usize) -> PathBuf {
        self.write(
            "outcomes.json",
            &format!(
                r#"{{"total_mutants":3,"caught":{},"missed":{missed},"timeout":0,"unviable":0,
                    "outcomes":[{{"summary":"MissedMutant","scenario":{{"Mutant":{{"name":"src/lib.rs: replace + with -"}}}}}}]}}"#,
                3 - missed
            ),
        )
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn init_writes_config_then_refuses_to_clobber_without_force() {
    let fx = Fixture::new();
    let out = fx.run(&["init"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("wrote rust-oleum.toml"));
    let config = fx.read("rust-oleum.toml");
    assert!(config.contains("rust_roots = [\"src\"]"));
    // Default targets are generous enough that the fixture has no debt.
    assert!(!config.contains("[baseline.cyclomatic]"));

    let again = fx.run(&["init"]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("--force"));

    let forced = fx.run(&["init", "--force"]);
    assert!(forced.status.success(), "{}", stderr(&forced));
}

#[test]
fn init_with_inputs_grandfathers_coverage_crap_and_lints() {
    let fx = Fixture::new();
    let lcov = fx.lcov();
    let log = fx.clippy_log();
    let out = fx.run(&[
        "init",
        "--coverage",
        lcov.to_str().unwrap(),
        "--clippy-log",
        log.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let config = fx.read("rust-oleum.toml");
    assert!(config.contains("coverage_min_pct = 24.5"), "{config}");
    // knotty: cc 4, 0% covered => CRAP 4^2 + 4 = 20, under the 25 target,
    // so no CRAP entry is grandfathered.
    assert!(!config.contains("[baseline.crap]"), "{config}");
    assert!(
        config.contains("\"unused_variables@src/lib.rs::tidy\" = 1"),
        "{config}"
    );

    // The config it wrote passes its own gate with the same inputs.
    let gate = fx.run(&[
        "--coverage",
        lcov.to_str().unwrap(),
        "--clippy-log",
        log.to_str().unwrap(),
    ]);
    assert!(gate.status.success(), "{}", stderr(&gate));
    assert!(stdout(&gate).contains("coverage input: 4 instrumented lines"));
}

#[test]
fn gate_passes_with_exit_zero_and_prints_the_table() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "[targets]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let out = fx.run(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Cyclomatic complexity (per fn)"));
    assert!(text.contains("not measured"));
    assert!(stderr(&out).is_empty());
}

#[test]
fn gate_fails_with_exit_one_and_writes_json_and_summary() {
    let fx = Fixture::new();
    fx.strict_config();
    let json = fx.root().join("report.json");
    let summary = fx.root().join("summary.md");
    std::fs::write(&summary, "existing\n").unwrap();
    let out = fx.run(&[
        "--json",
        json.to_str().unwrap(),
        "--github-summary",
        summary.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("quality gate violations:"), "{err}");
    assert!(
        err.contains("cyclomatic: src/lib.rs::knotty = 4 exceeds target 2 (new violation)"),
        "{err}"
    );

    let report: serde_json::Value = serde_json::from_str(&fx.read("report.json")).unwrap();
    assert_eq!(report["violations"].as_array().unwrap().len(), 2);
    assert_eq!(report["rows"][0]["passes_gate"], false);

    // Summary is appended, not truncated, and marks the failing rows.
    let md = fx.read("summary.md");
    assert!(
        md.starts_with("existing\n### rust-oleum quality gate"),
        "{md}"
    );
    assert!(
        md.contains("| Cyclomatic complexity (per fn) | 4 | <= 2 | ⚠️ | ❌ |"),
        "{md}"
    );
    assert!(md.contains("**Violations:**"), "{md}");
}

#[test]
fn github_summary_without_violations_has_no_violations_section() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "[targets]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let summary = fx.root().join("summary.md");
    let out = fx.run(&["--github-summary", summary.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let md = fx.read("summary.md");
    assert!(md.contains("| ✅ | ✅ |"), "{md}");
    assert!(!md.contains("**Violations:**"), "{md}");
}

#[test]
fn coverage_and_mutants_inputs_gate_their_rows() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "[targets]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let lcov = fx.lcov();
    let mutants = fx.mutants(1);
    let out = fx.run(&[
        "--coverage",
        lcov.to_str().unwrap(),
        "--mutants",
        mutants.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("coverage: 25.00% is below the enforced floor 100.00%"),
        "{err}"
    );
    assert!(!err.contains("crap:"), "{err}");
    assert!(
        err.contains("mutants: 1 surviving mutants (max 0): src/lib.rs: replace + with -"),
        "{err}"
    );
    let text = stdout(&out);
    assert!(text.contains("3 tested, 2 caught, 0 unviable"), "{text}");

    // With every mutant caught the mutants row passes.
    let mutants = fx.mutants(0);
    let out = fx.run(&["--mutants", mutants.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn clippy_log_findings_are_gated_per_item() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "[targets]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let log = fx.clippy_log();
    let out = fx.run(&["--clippy-log", log.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("dead code: unused_variables@src/lib.rs::tidy = 1 exceeds target 0"),
        "{}",
        stderr(&out)
    );

    // An empty log (nothing rebuilt) is rejected rather than passing.
    let empty = fx.write("empty.json", "");
    let out = fx.run(&["--clippy-log", empty.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("warm cargo cache"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn write_baseline_prints_a_section_without_gating() {
    let fx = Fixture::new();
    fx.strict_config();
    let out = fx.run(&["--write-baseline"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("[baseline]\n"), "{text}");
    assert!(
        text.contains("[baseline.cyclomatic]\n\"src/lib.rs::knotty\" = 4"),
        "{text}"
    );
    assert!(
        text.contains("[baseline.cognitive]\n\"src/lib.rs::knotty\" = 9"),
        "{text}"
    );
    // Pasting it back yields a passing config.
    let config = format!("{}{}", fx.read("rust-oleum.toml"), text);
    fx.write("rust-oleum.toml", &config);
    assert!(fx.run(&[]).status.success());
}

#[test]
fn tighten_rewrites_the_config_in_place() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "# keep me\n[targets]\ncyclomatic_max = 2\ncognitive_max = 2\n\n[sources]\nrust_roots = [\"src\"]\n\n\
         [baseline]\n\n[baseline.cyclomatic]\n\"src/lib.rs::knotty\" = 99\n\"src/lib.rs::gone\" = 7\n\n\
         [baseline.cognitive]\n\"src/lib.rs::knotty\" = 9\n",
    );
    // Loose and unmatched entries fail the gate first.
    let gate = fx.run(&[]);
    assert_eq!(gate.status.code(), Some(1));
    assert!(stderr(&gate).contains("below its grandfathered ceiling 99"));
    assert!(stderr(&gate).contains("matches no function"));

    let out = fx.run(&["--tighten"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("tightened baseline: 1 entries removed, 1 lowered"),
        "{}",
        stdout(&out)
    );
    let config = fx.read("rust-oleum.toml");
    assert!(config.starts_with("# keep me\n"), "{config}");
    assert!(config.contains("\"src/lib.rs::knotty\" = 4\n"), "{config}");
    assert!(!config.contains("gone"), "{config}");
    assert!(fx.run(&[]).status.success());
}

#[test]
fn tighten_refuses_a_config_with_tables_after_the_baseline() {
    let fx = Fixture::new();
    fx.write(
        "rust-oleum.toml",
        "[targets]\n\n[baseline]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let before = fx.read("rust-oleum.toml");
    let out = fx.run(&["--tighten"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("move the [baseline] tables to the end of the file"),
        "{}",
        stderr(&out)
    );
    assert_eq!(fx.read("rust-oleum.toml"), before);
}

#[test]
fn custom_config_path_and_missing_config_error() {
    let fx = Fixture::new();
    fx.write(
        "alt.toml",
        "[targets]\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    assert!(fx.run(&["--config", "alt.toml"]).status.success());

    let out = fx.run(&[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("reading config"), "{}", stderr(&out));
}

#[test]
fn bare_panics_are_gated_alongside_the_unchanged_panic_metric() {
    let fx = Fixture::new();
    fx.write(
        "src/lib.rs",
        "pub fn a(x: Option<u8>) -> u8 { x.expect(\"caller checked is_some\") }\n\
         pub fn b(x: Option<u8>) -> u8 { x.unwrap() }\n",
    );
    // A config written before bare_panic existed: panics grandfathered via
    // [baseline.panic] still load and still pass the panic row.
    fx.write(
        "rust-oleum.toml",
        "[targets]\npanic_max = 0\n\n[sources]\nrust_roots = [\"src\"]\n\n\
         [baseline.panic]\n\"src/lib.rs::a\" = 1\n\"src/lib.rs::b\" = 1\n",
    );
    let out = fx.run(&[]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("bare_panic: src/lib.rs::b = 1 exceeds target 0 (new violation)"),
        "{err}"
    );
    assert!(!err.contains("src/lib.rs::a"), "{err}");
    assert!(!err.contains("  - panic:"), "{err}");

    // The regenerated baseline grandfathers only the unexplained one.
    let text = stdout(&fx.run(&["--write-baseline"]));
    assert!(
        text.contains("[baseline.bare_panic]\n\"src/lib.rs::b\" = 1\n"),
        "{text}"
    );

    // Allowing explained panics: only the bare `.unwrap()` still fails.
    fx.write(
        "rust-oleum.toml",
        "[targets]\npanic_max = 1000\nbare_panic_max = 0\n\n[sources]\nrust_roots = [\"src\"]\n",
    );
    let out = fx.run(&[]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(err.contains("bare_panic: src/lib.rs::b = 1"), "{err}");
    assert_eq!(err.matches("\n  - ").count(), 1, "{err}");
    let table = stdout(&out);
    assert!(table.contains("Unexplained panics (per fn)"), "{table}");
}
