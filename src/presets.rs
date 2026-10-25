//! Lint group presets for `rust-oleum init --preset`.
//!
//! A preset is a set of `[lint_groups]` tables plus the clippy setup that
//! makes those lints show up in the `--clippy-log` (most are
//! allow-by-default, and an unemitted lint can't be ratcheted).

use std::fmt::Write;

/// A named lint group inside a preset.
struct GroupDef {
    name: &'static str,
    lints: &'static [&'static str],
}

/// The machine-checkable rules of BeautifulRust
/// (<https://github.com/morganlinton/BeautifulRust>, MIT), grouped by intent.
const BEAUTIFUL_RUST: &[GroupDef] = &[
    GroupDef {
        name: "panics",
        lints: &["clippy::unwrap_used", "clippy::panic_in_result_fn"],
    },
    GroupDef {
        name: "docs",
        lints: &[
            "missing_docs",
            "clippy::missing_errors_doc",
            "clippy::missing_panics_doc",
        ],
    },
    GroupDef {
        name: "api",
        lints: &[
            "clippy::fn_params_excessive_bools",
            "clippy::struct_excessive_bools",
            "clippy::ptr_arg",
            "missing_debug_implementations",
        ],
    },
    GroupDef {
        name: "casts",
        lints: &[
            "clippy::as_conversions",
            "clippy::cast_possible_truncation",
            "clippy::cast_sign_loss",
        ],
    },
    GroupDef {
        name: "unsafe_hygiene",
        lints: &[
            "clippy::undocumented_unsafe_blocks",
            "clippy::multiple_unsafe_ops_per_block",
            "unsafe_op_in_unsafe_fn",
        ],
    },
    GroupDef {
        name: "suppressions",
        lints: &[
            "clippy::allow_attributes",
            "clippy::allow_attributes_without_reason",
        ],
    },
    GroupDef {
        name: "leftovers",
        lints: &[
            "clippy::dbg_macro",
            "clippy::todo",
            "clippy::print_stdout",
            "clippy::print_stderr",
        ],
    },
];

/// `clippy.toml` settings the BeautifulRust preset relies on.
const BEAUTIFUL_RUST_CLIPPY_TOML: &str = "\
max-fn-params-bools = 0
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-print-in-tests = true
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Preset {
    /// Lint groups mirroring BeautifulRust's machine-checkable rules.
    BeautifulRust,
}

impl Preset {
    fn groups(self) -> &'static [GroupDef] {
        match self {
            Preset::BeautifulRust => BEAUTIFUL_RUST,
        }
    }

    fn clippy_toml(self) -> &'static str {
        match self {
            Preset::BeautifulRust => BEAUTIFUL_RUST_CLIPPY_TOML,
        }
    }

    /// The `[lint_groups.*]` tables for the config file.
    pub fn toml(self) -> String {
        let mut out = String::new();
        for g in self.groups() {
            let lints: Vec<String> = g.lints.iter().map(|l| format!("\"{l}\"")).collect();
            let _ = writeln!(out, "[lint_groups.{}]", g.name);
            let _ = writeln!(out, "lints = [{}]\n", lints.join(", "));
        }
        out
    }

    /// Every lint in the preset, in group order.
    fn lints(self) -> impl Iterator<Item = &'static str> {
        self.groups().iter().flat_map(|g| g.lints.iter().copied())
    }

    /// A `cargo clippy` invocation that emits every lint of the preset.
    pub fn clippy_command(self) -> String {
        let flags: Vec<String> = self.lints().map(|l| format!("-W {l}")).collect();
        format!(
            "cargo clippy --all-targets --message-format=json -- {} > clippy-log.json",
            flags.join(" ")
        )
    }

    /// What `init` prints after writing a config with this preset.
    pub fn setup_notes(self) -> String {
        let mut out = format!(
            "  lint groups: {} (preset inspired by https://github.com/morganlinton/BeautifulRust, MIT)\n\
             most of these lints are off by default, so enable them when producing the clippy log:\n  {}\n\
             and put this in clippy.toml next to Cargo.toml:\n",
            self.groups()
                .iter()
                .map(|g| g.name)
                .collect::<Vec<_>>()
                .join(", "),
            self.clippy_command(),
        );
        for line in self.clippy_toml().lines() {
            let _ = writeln!(out, "  {line}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn beautiful_rust_groups_load_without_overlap() {
        let p = Preset::BeautifulRust;
        let text = format!("[targets]\n{}[sources]\nrust_roots = []\n", p.toml());
        let config = Config::parse(&text).unwrap();
        let names: Vec<&str> = config.lint_groups.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "api",
                "casts",
                "docs",
                "leftovers",
                "panics",
                "suppressions",
                "unsafe_hygiene"
            ]
        );
        assert_eq!(config.lint_group_of("clippy::unwrap_used"), Some("panics"));
        assert_eq!(config.lint_group_of("missing_docs"), Some("docs"));
        assert_eq!(
            config.lint_group_of("clippy::print_stderr"),
            Some("leftovers")
        );
        assert!(config.lint_groups.values().all(|g| g.max == 0));
        assert_eq!(p.lints().count(), 21);
        assert!(p.toml().starts_with(
            "[lint_groups.panics]\nlints = [\"clippy::unwrap_used\", \"clippy::panic_in_result_fn\"]\n\n"
        ));
    }

    #[test]
    fn setup_notes_name_the_clippy_command_and_config() {
        let p = Preset::BeautifulRust;
        let cmd = p.clippy_command();
        assert!(cmd.starts_with(
            "cargo clippy --all-targets --message-format=json -- -W clippy::unwrap_used -W clippy::panic_in_result_fn -W missing_docs "
        ));
        assert!(
            cmd.ends_with("-W clippy::print_stderr > clippy-log.json"),
            "{cmd}"
        );
        let notes = p.setup_notes();
        assert!(
            notes.starts_with("  lint groups: panics, docs, api, casts,"),
            "{notes}"
        );
        assert!(notes.contains(&format!("\n  {cmd}\n")), "{notes}");
        assert!(notes.contains("BeautifulRust, MIT"), "{notes}");
        assert!(notes.ends_with("  max-fn-params-bools = 0\n  allow-unwrap-in-tests = true\n  allow-expect-in-tests = true\n  allow-print-in-tests = true\n"), "{notes}");
    }
}
