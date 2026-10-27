//! Rust safety and API rows of the gate report, one per-item metric each.

use std::collections::BTreeMap;

use super::{item_metric_row, Report};
use crate::config::{function_key, Config};
use crate::rust_metrics::{FileMetrics, FunctionMetrics, RustMetrics};

/// One per-function or per-file `u32` row: (key, label, field, target,
/// baseline).
type Row<'a, T> = (
    &'a str,
    &'a str,
    fn(&T) -> u32,
    u32,
    &'a BTreeMap<String, u32>,
);

/// Rust safety and API rows: `unsafe`, panic calls, lint suppressions,
/// denied warning groups, wrapper types in pub signatures, `get_` getters,
/// and public error types without an `Error` impl.
pub(super) fn safety_rows(config: &Config, rust: &RustMetrics, report: &mut Report) {
    let t = &config.targets;
    let b = &config.baseline;
    let fn_rows: [Row<FunctionMetrics>; 7] = [
        (
            "unsafe",
            "Unsafe blocks (per fn)",
            |f| f.unsafe_count,
            t.unsafe_max,
            &b.unsafe_,
        ),
        (
            "panic",
            "Panic calls (per fn)",
            |f| f.panic_count,
            t.panic_max,
            &b.panic,
        ),
        (
            "bare_panic",
            "Unexplained panics (per fn)",
            |f| f.bare_panic_count,
            t.bare_panic_max,
            &b.bare_panic,
        ),
        (
            "allow",
            "Lint suppressions (per fn)",
            |f| f.allow_count,
            t.allow_max,
            &b.allow,
        ),
        (
            "bare_allow",
            "Unjustified allows (per fn)",
            |f| f.bare_allow_count,
            t.bare_allow_max,
            &b.bare_allow,
        ),
        (
            "pub_wrapper",
            "Rc/Arc/Box/.. in pub signatures",
            |f| f.pub_wrapper_count,
            t.pub_wrapper_max,
            &b.pub_wrapper,
        ),
        (
            "get_prefix",
            "get_ getters (per fn)",
            |f| f.get_prefix_count,
            t.get_prefix_max,
            &b.get_prefix,
        ),
    ];
    for (key, label, field, target, baseline) in fn_rows {
        let items = rust
            .functions
            .iter()
            .map(|f| (function_key(&f.file, &f.name), field(f)));
        item_metric_row(report, (key, "function"), label, items, target, baseline);
    }
    let file_rows: [Row<FileMetrics>; 3] = [
        (
            "file_allow",
            "Lint suppressions (per file)",
            |f| f.allow_count,
            t.file_allow_max,
            &b.file_allow,
        ),
        (
            "file_bare_allow",
            "Unjustified allows (per file)",
            |f| f.bare_allow_count,
            t.file_bare_allow_max,
            &b.file_bare_allow,
        ),
        (
            "deny_warnings",
            "Warning groups denied in source",
            |f| f.deny_warnings_count,
            t.deny_warnings_max,
            &b.deny_warnings,
        ),
    ];
    for (key, label, field, target, baseline) in file_rows {
        let items = rust.files.iter().map(|f| (f.file.clone(), field(f)));
        item_metric_row(report, (key, "file"), label, items, target, baseline);
    }
    item_metric_row(
        report,
        ("missing_error_impl", "type"),
        "Pub error types without Error",
        rust.error_types.iter().map(|(k, v)| (k.clone(), *v)),
        t.missing_error_impl_max,
        &b.missing_error_impl,
    );
}
