//! Rust-specific safety metrics: per-function `unsafe` usage, panic-prone
//! calls, and lint suppressions, plus file-level lint suppressions that are
//! not attached to any one function.
//!
//! Like the complexity metrics, these walk the function body as written:
//! closures and nested fns count toward the enclosing function. Macro
//! arguments are parsed as expressions where possible so that, e.g., an
//! `.unwrap()` inside `format!(..)` is still seen.

use syn::punctuated::Punctuated;
use syn::visit::Visit;

/// Macros that panic (or may panic) in release builds.
const PANIC_MACROS: &[&str] = &[
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "assert",
    "assert_eq",
    "assert_ne",
];

/// Methods that panic on the error / `None` path.
const PANIC_METHODS: &[&str] = &["unwrap", "expect"];

/// Per-function safety counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SafetyCounts {
    pub unsafe_count: u32,
    pub panic_count: u32,
    pub allow_count: u32,
}

/// Measure one function: its signature, its attributes (outer and inner),
/// and its body.
pub fn measure_fn(
    sig: &syn::Signature,
    attrs: &[syn::Attribute],
    body: &syn::Block,
) -> SafetyCounts {
    let mut v = BodyVisitor::default();
    v.visit_block(body);
    v.counts.unsafe_count += u32::from(sig.unsafety.is_some());
    v.counts.allow_count += attrs.iter().map(allow_lints).sum::<u32>();
    v.counts
}

/// Lint suppressions in a file that are not on a function: inner
/// `#![allow(..)]` attributes and `#[allow(..)]` / `#[expect(..)]` on mods,
/// impls, types, fields, and other non-fn items. Test modules are skipped.
pub fn file_allow_count(ast: &syn::File) -> u32 {
    let mut v = FileAllowVisitor { count: 0 };
    v.visit_file(ast);
    v.count
}

/// Number of lints suppressed by one attribute: each lint path inside
/// `#[allow(..)]` / `#[expect(..)]`; `reason = ".."` does not count.
fn allow_lints(attr: &syn::Attribute) -> u32 {
    let syn::Meta::List(list) = &attr.meta else {
        return 0;
    };
    if !(list.path.is_ident("allow") || list.path.is_ident("expect")) {
        return 0;
    }
    match list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated) {
        Ok(metas) => {
            let lints = metas
                .iter()
                .filter(|m| matches!(m, syn::Meta::Path(_)))
                .count();
            u32::try_from(lints).unwrap_or(u32::MAX)
        }
        // Unparseable arguments still suppress something.
        Err(_) => 1,
    }
}

fn last_ident_is(path: &syn::Path, names: &[&str]) -> bool {
    path.segments
        .last()
        .is_some_and(|s| names.iter().any(|n| s.ident == n))
}

#[derive(Default)]
struct BodyVisitor {
    counts: SafetyCounts,
}

impl<'ast> Visit<'ast> for BodyVisitor {
    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.counts.unsafe_count += 1;
        syn::visit::visit_expr_unsafe(self, node);
    }

    fn visit_signature(&mut self, node: &'ast syn::Signature) {
        // Nested `unsafe fn` items inside the body.
        self.counts.unsafe_count += u32::from(node.unsafety.is_some());
        syn::visit::visit_signature(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if PANIC_METHODS.iter().any(|m| node.method == m) {
            self.counts.panic_count += 1;
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if last_ident_is(&node.path, PANIC_MACROS) {
            self.counts.panic_count += 1;
        }
        // Look inside macro arguments that parse as expressions.
        if let Ok(args) =
            node.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated)
        {
            for arg in &args {
                self.visit_expr(arg);
            }
        }
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        self.counts.allow_count += allow_lints(node);
    }
}

struct FileAllowVisitor {
    count: u32,
}

impl<'ast> Visit<'ast> for FileAllowVisitor {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        self.count += allow_lints(node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !crate::rust_metrics::is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !crate::rust_metrics::is_cfg_test(&node.attrs) {
            syn::visit::visit_item_impl(self, node);
        }
    }

    // Functions with bodies are measured per function; skip them entirely.
    fn visit_item_fn(&mut self, _node: &'ast syn::ItemFn) {}

    fn visit_impl_item_fn(&mut self, _node: &'ast syn::ImplItemFn) {}

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        if node.default.is_none() {
            syn::visit::visit_trait_item_fn(self, node);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fn_counts(src: &str) -> SafetyCounts {
        let item: syn::ItemFn = syn::parse_str(src).unwrap();
        measure_fn(&item.sig, &item.attrs, &item.block)
    }

    fn file_count(src: &str) -> u32 {
        file_allow_count(&syn::parse_file(src).unwrap())
    }

    #[test]
    fn clean_fn_scores_zero() {
        assert_eq!(fn_counts("fn f() { let x = 1; }"), SafetyCounts::default());
    }

    #[test]
    fn unsafe_fn_and_unsafe_blocks() {
        assert_eq!(fn_counts("unsafe fn f() {}").unsafe_count, 1);
        assert_eq!(
            fn_counts("fn f() { unsafe { a() }; let x = unsafe { b() }; }").unsafe_count,
            2
        );
        assert_eq!(
            fn_counts("unsafe fn f() { unsafe { a() } }").unsafe_count,
            2
        );
        assert_eq!(fn_counts("fn f() { unsafe fn inner() {} }").unsafe_count, 1);
        assert_eq!(fn_counts("fn f() { let s = \"unsafe\"; }").unsafe_count, 0);
    }

    #[test]
    fn panic_calls_are_counted() {
        let c = fn_counts(
            r#"fn f() {
                a.unwrap();
                b.expect("msg");
                panic!("boom");
                std::unreachable!();
                todo!();
                unimplemented!();
                assert!(x);
                assert_eq!(a, b);
                let y = { assert_ne!(a, b); 1 };
            }"#,
        );
        assert_eq!(c.panic_count, 9);
    }

    #[test]
    fn non_panicking_lookalikes_are_not_counted() {
        let c = fn_counts(
            r#"fn f() {
                a.unwrap_or(1);
                a.unwrap_or_default();
                a.unwrap_or_else(|| 2);
                a.expect_err("x");
                debug_assert!(x);
                debug_assert_eq!(a, b);
                unwrap(a);
                println!("unwrap");
            }"#,
        );
        assert_eq!(c.panic_count, 0);
    }

    #[test]
    fn panics_inside_macro_args_and_closures_are_counted() {
        let c = fn_counts(
            r#"fn f() {
                println!("{}", a.unwrap());
                let g = || b.expect("x");
                assert!(c.unwrap());
            }"#,
        );
        assert_eq!(c.panic_count, 4);
    }

    #[test]
    fn allow_counts_each_lint_on_fn_and_in_body() {
        assert_eq!(
            fn_counts("#[allow(dead_code, unused)] fn f() {}").allow_count,
            2
        );
        assert_eq!(
            fn_counts("#[allow(clippy::too_many_lines)] fn f() {}").allow_count,
            1
        );
        let body = r#"fn f() {
            #[allow(unused_variables)]
            let x = 1;
            #[expect(clippy::cast_possible_truncation)]
            let y = z as u8;
        }"#;
        assert_eq!(fn_counts(body).allow_count, 2);
        // Inner attributes in the body land on the fn's attrs in syn.
        assert_eq!(fn_counts("fn f() { #![allow(unused)] }").allow_count, 1);
        // Unrelated attributes don't count.
        let other = "#[inline] #[must_use] #[cfg_attr(test, allow(x))] fn f() {}";
        assert_eq!(fn_counts(other).allow_count, 0);
    }

    #[test]
    fn expect_attribute_ignores_reason() {
        let src = r#"#[expect(dead_code, reason = "wip")] fn f() {}"#;
        assert_eq!(fn_counts(src).allow_count, 1);
    }

    #[test]
    fn file_level_allows_exclude_functions_and_tests() {
        let src = r#"
            #![allow(clippy::module_name_repetitions, unused)]
            #[allow(dead_code)]
            struct S { #[allow(unused)] field: u8 }
            #[allow(clippy::new_without_default)]
            impl S {
                #[allow(dead_code)]
                fn per_fn() {}
                #[allow(unused)]
                const C: u8 = 1;
            }
            mod m { #![allow(missing_docs)] }
            trait T {
                #[allow(unused)]
                fn required(&self);
                #[allow(unused)]
                fn provided(&self) {}
            }
            #[allow(dead_code)]
            fn top() {}
            #[cfg(test)]
            mod tests { #![allow(unused)] }
            #[cfg(test)]
            #[allow(unused)]
            impl S {}
        "#;
        // 2 inner + struct + field + impl + const + mod + required = 8.
        assert_eq!(file_count(src), 8);
        assert_eq!(file_count("fn f() {}"), 0);
    }

    #[test]
    fn unparseable_allow_counts_once() {
        let attr: syn::Attribute = syn::parse_quote!(#[allow(=)]);
        assert_eq!(allow_lints(&attr), 1);
        let attr: syn::Attribute = syn::parse_quote!(#[allow]);
        assert_eq!(allow_lints(&attr), 0);
    }

    #[test]
    fn only_non_test_modules_count_toward_file_allow() {
        assert_eq!(file_count("mod m { #![allow(unused)] }"), 1);
        assert_eq!(file_count("#[cfg(test)] mod t { #![allow(unused)] }"), 0);
    }
}
