//! Source-level metrics for Rust production code: per-function cyclomatic
//! complexity, cognitive complexity (Sonar-style approximation), Halstead
//! difficulty, per-file logical lines of code, and the Rust safety counts
//! from [`crate::rust_safety`] (unsafe, panics, lint suppressions) and the
//! public-API checks from [`crate::rust_api`].
//!
//! Test code (`tests/` dirs, `*_tests.rs` / `tests.rs` files, `#[cfg(test)]`
//! modules, `#[test]` functions) is excluded: the benchmark measures the
//! shipping surface, and test tables/fixtures legitimately trade these
//! metrics for readability.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use syn::spanned::Spanned;
use syn::visit::Visit;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FunctionMetrics {
    /// Repo-relative file path.
    pub file: String,
    /// Qualified name within the file, e.g. `LedgerHandle::summary`.
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub halstead_difficulty: f64,
    /// `unsafe` blocks in the body, plus one for an `unsafe fn`.
    pub unsafe_count: u32,
    /// `.unwrap()` / `.expect(..)` calls and panicking macros.
    pub panic_count: u32,
    /// Lints suppressed by `#[allow]` / `#[expect]` on or inside the fn.
    pub allow_count: u32,
    /// Panics that don't say why they can't happen (`.unwrap()`, `todo!()`,
    /// `assert!(c)` without a message, ...).
    pub bare_panic_count: u32,
    /// Suppressions not written as `#[expect(lint, reason = "..")]`.
    pub bare_allow_count: u32,
    /// `Rc` / `RefCell` / `Arc` / `Box` / `Cell` / `Mutex` in a `pub`
    /// signature.
    pub pub_wrapper_count: u32,
    /// 1 for a `pub fn get_x(&self)` getter.
    pub get_prefix_count: u32,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FileMetrics {
    pub file: String,
    /// Non-blank, non-comment lines, test modules excluded.
    pub loc: usize,
    /// Lints suppressed outside functions (inner `#![allow]`, item attrs).
    pub allow_count: u32,
    /// The subset of `allow_count` not written as `#[expect(.., reason)]`.
    pub bare_allow_count: u32,
    /// Warning groups (`warnings`, `clippy::all`, ...) denied in source.
    pub deny_warnings_count: u32,
}

#[derive(Debug, Default)]
pub struct RustMetrics {
    pub files: Vec<FileMetrics>,
    pub functions: Vec<FunctionMetrics>,
    /// `"file::Type"` -> 1 for a public error type (defined in the crate)
    /// without an `Error` impl, 0 for one with it.
    pub error_types: std::collections::BTreeMap<String, u32>,
}

/// True for files that hold test code rather than shipping code.
fn is_test_path(rel: &str) -> bool {
    rel.contains("/tests/")
        || rel.contains("/benches/")
        || rel.ends_with("/tests.rs")
        || rel.ends_with("_tests.rs")
}

/// Collect metrics for every production `.rs` file under the given roots.
/// Each root is one crate for the crate-wide error-type scan.
pub fn collect(repo_root: &Path, source_roots: &[String]) -> Result<RustMetrics> {
    let mut out = RustMetrics::default();
    for root in source_roots {
        let mut rust_files = Vec::new();
        collect_rust_files(&repo_root.join(root), &mut rust_files)?;
        rust_files.sort();
        collect_crate(repo_root, rust_files, &mut out)?;
    }
    Ok(out)
}

fn collect_crate(repo_root: &Path, rust_files: Vec<PathBuf>, out: &mut RustMetrics) -> Result<()> {
    let mut errors = crate::rust_api::ErrorTypeScan::default();
    for path in rust_files {
        let rel = path
            .strip_prefix(repo_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_path(&rel) {
            continue;
        }
        let src = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let ast: syn::File =
            syn::parse_file(&src).with_context(|| format!("parsing {}", path.display()))?;

        let safety = crate::rust_safety::measure_file(&ast);
        out.files.push(FileMetrics {
            file: rel.clone(),
            loc: count_loc(&src, &ast),
            allow_count: safety.allow_count,
            bare_allow_count: safety.bare_allow_count,
            deny_warnings_count: safety.deny_warnings_count,
        });
        errors.scan(&rel, &ast);

        let mut visitor = FnVisitor {
            file: rel,
            scope: Vec::new(),
            functions: &mut out.functions,
        };
        visitor.visit_file(&ast);
    }
    out.error_types.extend(errors.finish());
    Ok(())
}

fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            collect_rust_files(&path, out)?;
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Non-blank, non-comment lines, minus lines inside `#[cfg(test)]` modules.
fn count_loc(src: &str, ast: &syn::File) -> usize {
    let mut code_lines = crate::loc::code_lines(src);
    for item in &ast.items {
        if let syn::Item::Mod(m) = item {
            if is_cfg_test(&m.attrs) {
                let start = m.span().start().line;
                let end = m.span().end().line;
                code_lines.retain(|&l| l < start || l > end);
            }
        }
    }
    code_lines.len()
}

/// `#[cfg(test)]` (or any cfg predicate mentioning `test`).
pub(crate) fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| match &a.meta {
        syn::Meta::List(l) if l.path.is_ident("cfg") => {
            l.tokens.clone().into_iter().any(|t| match t {
                proc_macro2::TokenTree::Ident(i) => i == "test",
                _ => false,
            })
        }
        _ => false,
    })
}

/// `#[test]`, `#[tokio::test]`, and friends.
pub(crate) fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
        || is_cfg_test(attrs)
}

struct FnVisitor<'a> {
    file: String,
    scope: Vec<String>,
    functions: &'a mut Vec<FunctionMetrics>,
}

impl FnVisitor<'_> {
    fn record(
        &mut self,
        (vis, sig): (&syn::Visibility, &syn::Signature),
        attrs: &[syn::Attribute],
        block: &syn::Block,
        span: proc_macro2::Span,
    ) {
        if has_test_attr(attrs) {
            return;
        }
        let name = sig.ident.to_string();
        let safety = crate::rust_safety::measure_fn(sig, attrs, block);
        let qualified = if self.scope.is_empty() {
            name
        } else {
            format!("{}::{}", self.scope.join("::"), name)
        };
        self.functions.push(FunctionMetrics {
            file: self.file.clone(),
            name: qualified,
            start_line: span.start().line,
            end_line: span.end().line,
            cyclomatic: crate::complexity::cyclomatic(block),
            cognitive: crate::complexity::cognitive(block),
            halstead_difficulty: crate::complexity::halstead_difficulty(block),
            unsafe_count: safety.unsafe_count,
            panic_count: safety.panic_count,
            allow_count: safety.allow_count,
            bare_panic_count: safety.bare_panic_count,
            bare_allow_count: safety.bare_allow_count,
            pub_wrapper_count: crate::rust_api::pub_wrapper_count(vis, sig),
            get_prefix_count: crate::rust_api::get_prefix_count(vis, sig),
        });
    }
}

impl<'ast> Visit<'ast> for FnVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.scope.push(node.ident.to_string());
        syn::visit::visit_item_mod(self, node);
        self.scope.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // A `#[cfg(test)] impl` is test code; its methods carry no attr.
        if is_cfg_test(&node.attrs) {
            return;
        }
        self.scope.push(type_name_of(&node.self_ty));
        syn::visit::visit_item_impl(self, node);
        self.scope.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        // No recursion: closures and nested fns score as part of this body.
        self.record(
            (&node.vis, &node.sig),
            &node.attrs,
            &node.block,
            node.span(),
        );
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.record(
            (&node.vis, &node.sig),
            &node.attrs,
            &node.block,
            node.span(),
        );
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if let Some(block) = &node.default {
            let sig = (&syn::Visibility::Inherited, &node.sig);
            self.record(sig, &node.attrs, block, node.span());
        }
    }
}

pub(crate) fn type_name_of(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_else(|| "impl".into()),
        syn::Type::Reference(r) => type_name_of(&r.elem),
        _ => "impl".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics_for(src: &str) -> Vec<FunctionMetrics> {
        let ast: syn::File = syn::parse_file(src).unwrap();
        let mut functions = Vec::new();
        let mut v = FnVisitor {
            file: "test.rs".into(),
            scope: Vec::new(),
            functions: &mut functions,
        };
        v.visit_file(&ast);
        functions
    }

    #[test]
    fn skips_cfg_test_modules_and_test_fns() {
        let fns = metrics_for(
            r#"
            fn shipped() {}
            #[cfg(test)]
            mod tests {
                fn helper() {}
            }
            #[test]
            fn a_test() {}
            "#,
        );
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "shipped");
    }

    #[test]
    fn qualifies_impl_methods() {
        let fns = metrics_for("struct S; impl S { fn go(&self) {} }");
        assert_eq!(fns[0].name, "S::go");
    }

    #[test]
    fn skips_cfg_test_impl_blocks() {
        let fns = metrics_for(
            r#"
            struct S;
            impl S { fn shipped(&self) {} }
            #[cfg(test)]
            impl S { fn helper(&self) { x.unwrap() } }
            "#,
        );
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["S::shipped"]);
    }

    #[test]
    fn straight_line_fn_has_base_complexity() {
        let fns = metrics_for("fn f() { let x = 1; }");
        assert_eq!(fns[0].cyclomatic, 1);
        assert_eq!(fns[0].cognitive, 0);
    }

    #[test]
    fn is_test_path_variants() {
        assert!(is_test_path("crates/x/tests/smoke.rs"));
        assert!(is_test_path("crates/x/benches/b.rs"));
        assert!(is_test_path("crates/x/src/ledger/tests.rs"));
        assert!(is_test_path("crates/x/src/analyze/patterns_tests.rs"));
        assert!(!is_test_path("crates/x/src/lib.rs"));
        assert!(!is_test_path("crates/x/src/testsuite.rs"));
    }

    #[test]
    fn nested_modules_and_trait_defaults_are_collected() {
        let fns = metrics_for(
            r#"
            mod outer { mod inner { fn deep() {} } }
            trait T { fn provided(&self) { let _x = 1; } fn required(&self); }
            impl T for &Widget { fn required(&self) {} }
            #[cfg_attr(test, allow(dead_code))]
            fn kept() {}
            "#,
        );
        let names: Vec<&str> = fns.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["outer::inner::deep", "provided", "Widget::required", "kept"]
        );
    }

    #[test]
    fn count_loc_excludes_exactly_the_test_module_span() {
        // Asymmetric line counts on both sides of the module so an
        // off-by-one at either boundary changes the total.
        let src = "fn a() {}\nfn a2() {}\n#[cfg(test)]\nmod t {\n    fn b() {}\n}\nfn c() {}\nfn d() {}\n";
        let ast: syn::File = syn::parse_file(src).unwrap();
        // Lines 3-6 (attr through closing brace) are the test module;
        // lines 1, 2, 7, and 8 remain.
        assert_eq!(count_loc(src, &ast), 4);
    }

    #[test]
    fn collect_walks_only_production_rust_sources() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let w = |rel: &str, content: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };
        w("c/src/lib.rs", "fn shipped() { let x = 1; }\n");
        w("c/src/nested/m.rs", "fn inner() {}\n");
        w("c/src/tests.rs", "fn t() {}\n");
        w("c/src/foo_tests.rs", "fn t() {}\n");
        w("c/src/tests/fixture.rs", "fn t() {}\n");
        w("c/src/target/gen.rs", "fn t() {}\n");
        w("c/src/.hidden/h.rs", "fn t() {}\n");
        w("c/src/notes.txt", "fn not_rust() {}\n");

        let m = collect(root, &["c/src".to_string()]).unwrap();
        let files: Vec<&str> = m.files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(files, vec!["c/src/lib.rs", "c/src/nested/m.rs"]);
        assert_eq!(m.files[0].loc, 1);
        let names: Vec<&str> = m.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["shipped", "inner"]);

        let empty = collect(root, &["c/absent".to_string()]).unwrap();
        assert!(empty.files.is_empty());
    }

    #[test]
    fn collect_records_api_counts_and_scans_errors_per_crate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let w = |rel: &str, content: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        };
        // Same error name in two crates; only `b` implements `Error`, which
        // must not excuse `a`.
        w(
            "a/src/lib.rs",
            "#![deny(warnings)]\n#[allow(dead_code)]\npub struct E;\n\
             pub fn f() -> Result<(), E> { Ok(()) }\n\
             pub struct S;\nimpl S { pub fn get_b(&self) -> Box<u8> { x.unwrap() } }\n\
             trait T { fn get_t(&self) -> Box<u8> { y.expect(\"why\") } }\n",
        );
        w(
            "b/src/lib.rs",
            "pub struct E;\nimpl std::error::Error for E {}\npub fn g() -> Result<(), E> { Ok(()) }\n",
        );
        let m = collect(root, &["a/src".to_string(), "b/src".to_string()]).unwrap();
        assert_eq!(
            m.error_types,
            std::collections::BTreeMap::from([
                ("a/src/lib.rs::E".to_string(), 1),
                ("b/src/lib.rs::E".to_string(), 0),
            ])
        );
        let a = &m.files[0];
        assert_eq!(
            (a.allow_count, a.bare_allow_count, a.deny_warnings_count),
            (1, 1, 1)
        );
        let get_b = m.functions.iter().find(|f| f.name == "S::get_b").unwrap();
        assert_eq!((get_b.pub_wrapper_count, get_b.get_prefix_count), (1, 1));
        assert_eq!((get_b.panic_count, get_b.bare_panic_count), (1, 1));
        // Trait default methods have no visibility of their own.
        let get_t = m.functions.iter().find(|f| f.name == "get_t").unwrap();
        assert_eq!((get_t.pub_wrapper_count, get_t.get_prefix_count), (0, 0));
        assert_eq!((get_t.panic_count, get_t.bare_panic_count), (1, 0));
        let f = m.functions.iter().find(|f| f.name == "f").unwrap();
        assert_eq!((f.pub_wrapper_count, f.get_prefix_count), (0, 0));
    }
}
