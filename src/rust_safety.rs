//! Rust-specific safety metrics: per-function `unsafe` usage, panic-prone
//! calls, and lint suppressions, plus file-level lint suppressions that are
//! not attached to any one function and source-level `deny` of warning
//! groups.
//!
//! Like the complexity metrics, these walk the function body as written:
//! closures and nested fns count toward the enclosing function. Macro
//! arguments are parsed as expressions where possible so that, e.g., an
//! `.unwrap()` inside `format!(..)` is still seen.
//!
//! Panics and suppressions are counted twice: every occurrence (the original
//! `panic` / `allow` metrics), and the unexplained subset (`bare_panic` /
//! `bare_allow`). A panic is explained when it says why it cannot happen
//! (`.expect("reason")`, `assert!(cond, "msg")`, `panic!("msg")`); a
//! suppression is justified only as `#[expect(lint, reason = "..")]`, which
//! also warns once the lint stops firing. The bare counts are a subset of the
//! totals, so a project can keep `panic_max = 0` (no panics at all) or raise
//! it and gate only on `bare_panic_max`.

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

/// Methods that panic on an unexpected variant (including `Ok`).
const PANIC_METHODS: &[&str] = &["unwrap", "expect", "unwrap_err", "expect_err"];

/// Lint groups whose membership grows with the toolchain: denying them in
/// source turns a compiler upgrade into a broken build. Deny them in CI
/// (`-D warnings`, `[lints]` in Cargo.toml) instead.
const WARNING_GROUPS: &[&str] = &[
    "warnings",
    "unused",
    "nonstandard_style",
    "rust_2018_idioms",
    "future_incompatible",
    "clippy::all",
    "clippy::pedantic",
    "clippy::nursery",
    "clippy::cargo",
    "clippy::style",
    "clippy::complexity",
    "clippy::perf",
    "clippy::suspicious",
    "clippy::correctness",
    "clippy::restriction",
];

/// Per-function safety counts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SafetyCounts {
    pub unsafe_count: u32,
    pub panic_count: u32,
    /// Panics that don't say why they cannot happen (subset of `panic_count`).
    pub bare_panic_count: u32,
    pub allow_count: u32,
    /// Suppressions other than `#[expect(.., reason = "..")]` (subset of
    /// `allow_count`).
    pub bare_allow_count: u32,
}

/// File-level counts: everything outside function bodies.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FileCounts {
    pub allow_count: u32,
    pub bare_allow_count: u32,
    /// Warning groups denied (or forbidden) in source.
    pub deny_warnings_count: u32,
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
    for attr in attrs {
        v.add_suppression(attr);
    }
    v.counts
}

/// Lint suppressions in a file that are not on a function: inner
/// `#![allow(..)]` attributes and `#[allow(..)]` / `#[expect(..)]` on mods,
/// impls, types, fields, and other non-fn items; plus `deny` / `forbid` of
/// warning groups in those same attributes. Test modules are skipped.
pub fn measure_file(ast: &syn::File) -> FileCounts {
    let mut v = FileAllowVisitor::default();
    v.visit_file(ast);
    v.counts
}

/// Lints suppressed by one attribute, and whether the suppression is
/// justified (`#[expect(..)]` with a non-blank `reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Suppression {
    lints: u32,
    justified: bool,
}

impl Suppression {
    fn bare_lints(self) -> u32 {
        if self.justified {
            0
        } else {
            self.lints
        }
    }
}

/// Parse `#[name(a, b = "..")]` into its metas: `None` when the attribute is
/// not a `name(..)` list, `Some(Err)` when its arguments don't parse.
fn list_args(
    attr: &syn::Attribute,
    names: &[&str],
) -> Option<syn::Result<Punctuated<syn::Meta, syn::Token![,]>>> {
    let syn::Meta::List(list) = &attr.meta else {
        return None;
    };
    if !names.iter().any(|n| list.path.is_ident(n)) {
        return None;
    }
    Some(list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated))
}

/// Each lint path inside `#[allow(..)]` / `#[expect(..)]` counts once;
/// `reason = ".."` does not count, but justifies an `#[expect]`.
fn suppression(attr: &syn::Attribute) -> Suppression {
    let none = Suppression {
        lints: 0,
        justified: false,
    };
    match list_args(attr, &["allow", "expect"]) {
        None => none,
        Some(Ok(metas)) => {
            let lints = metas
                .iter()
                .filter(|m| matches!(m, syn::Meta::Path(_)))
                .count();
            Suppression {
                lints: u32::try_from(lints).unwrap_or(u32::MAX),
                justified: attr.path().is_ident("expect") && metas.iter().any(is_reason),
            }
        }
        // Unparseable arguments still suppress something.
        Some(Err(_)) => Suppression { lints: 1, ..none },
    }
}

/// `reason = "<non-blank>"`.
fn is_reason(meta: &syn::Meta) -> bool {
    let syn::Meta::NameValue(nv) = meta else {
        return false;
    };
    nv.path.is_ident("reason") && str_literal(&nv.value).is_some_and(|s| !s.trim().is_empty())
}

/// The value of a string literal expression.
fn str_literal(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// A string literal that is empty or whitespace-only.
fn is_blank_str(expr: &syn::Expr) -> bool {
    str_literal(expr).is_some_and(|s| s.trim().is_empty())
}

/// Warning groups denied or forbidden by one attribute, looking inside
/// `cfg_attr(pred, deny(..))` too, however deeply nested.
fn denied_warning_groups(attr: &syn::Attribute) -> u32 {
    denied_in_meta(&attr.meta)
}

fn denied_in_meta(meta: &syn::Meta) -> u32 {
    let syn::Meta::List(list) = meta else {
        return 0;
    };
    if list.path.is_ident("cfg_attr") {
        // The first argument is the cfg predicate; the rest are attributes.
        return list
            .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            .map_or(0, |args| args.iter().skip(1).map(denied_in_meta).sum());
    }
    if !(list.path.is_ident("deny") || list.path.is_ident("forbid")) {
        return 0;
    }
    // Parse as metas so a trailing `reason = ".."` doesn't fail the parse.
    let Ok(metas) = list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
    else {
        return 0;
    };
    let groups = metas
        .iter()
        .filter(|m| match m {
            syn::Meta::Path(p) => WARNING_GROUPS.contains(&path_string(p).as_str()),
            _ => false,
        })
        .count();
    u32::try_from(groups).unwrap_or(u32::MAX)
}

fn path_string(path: &syn::Path) -> String {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    segments.join("::")
}

/// Whether a panicking macro call leaves its reason unstated: `todo!` /
/// `unimplemented!` always do; `panic!` / `unreachable!` without a message;
/// the `assert` family without the optional trailing message argument. A
/// blank string literal is no message.
fn is_bare_panic_macro(name: &str, args: &[syn::Expr]) -> bool {
    let message_at = match name {
        "todo" | "unimplemented" => return true,
        "assert" => 1,
        "assert_eq" | "assert_ne" => 2,
        _ => 0,
    };
    args.get(message_at).is_none_or(is_blank_str)
}

/// `.unwrap()`, or `.expect(..)` with no or a blank message.
fn is_bare_panic_method(node: &syn::ExprMethodCall) -> bool {
    node.method == "unwrap" || node.args.first().is_none_or(is_blank_str)
}

#[derive(Default)]
struct BodyVisitor {
    counts: SafetyCounts,
}

impl BodyVisitor {
    fn add_suppression(&mut self, attr: &syn::Attribute) {
        let s = suppression(attr);
        self.counts.allow_count += s.lints;
        self.counts.bare_allow_count += s.bare_lints();
    }

    fn add_panic(&mut self, bare: bool) {
        self.counts.panic_count += 1;
        self.counts.bare_panic_count += u32::from(bare);
    }
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
            self.add_panic(is_bare_panic_method(node));
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        // Look inside macro arguments. A comma-separated expression list
        // (`format!("{}", x)`) is parsed with the real grammar, so commas in
        // turbofish or closure parameters stay intact. Bodies that are not
        // such a list (`vec![x; n]`) fall back to a split on top-level `,`
        // and `;`, visiting every piece that parses as an expression.
        let pieces =
            match node.parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated) {
                Ok(args) => args.into_iter().collect(),
                Err(_) => split_macro_args(node.tokens.clone())
                    .into_iter()
                    .filter_map(|piece| syn::parse2::<syn::Expr>(piece).ok())
                    .collect::<Vec<_>>(),
            };
        if let Some(name) = panic_macro_name(&node.path) {
            self.add_panic(is_bare_panic_macro(&name, &pieces));
        }
        for expr in &pieces {
            self.visit_expr(expr);
        }
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        self.add_suppression(node);
    }
}

/// The macro's name when it is one of [`PANIC_MACROS`].
fn panic_macro_name(path: &syn::Path) -> Option<String> {
    path.segments
        .last()
        .map(|s| s.ident.to_string())
        .filter(|n| PANIC_MACROS.contains(&n.as_str()))
}

/// Split a macro body into the token runs between top-level `,` and `;`.
fn split_macro_args(tokens: proc_macro2::TokenStream) -> Vec<proc_macro2::TokenStream> {
    let mut pieces = Vec::new();
    let mut current = proc_macro2::TokenStream::new();
    for tree in tokens {
        match &tree {
            proc_macro2::TokenTree::Punct(p) if p.as_char() == ',' || p.as_char() == ';' => {
                pieces.push(std::mem::take(&mut current));
            }
            _ => current.extend(std::iter::once(tree)),
        }
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
}

#[derive(Default)]
struct FileAllowVisitor {
    counts: FileCounts,
}

impl<'ast> Visit<'ast> for FileAllowVisitor {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        let s = suppression(node);
        self.counts.allow_count += s.lints;
        self.counts.bare_allow_count += s.bare_lints();
        self.counts.deny_warnings_count += denied_warning_groups(node);
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

    fn file_counts(src: &str) -> FileCounts {
        measure_file(&syn::parse_file(src).unwrap())
    }

    fn file_count(src: &str) -> u32 {
        file_counts(src).allow_count
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
                c.unwrap_err();
                d.expect_err("msg");
                panic!("boom");
                std::unreachable!();
                todo!();
                unimplemented!();
                assert!(x);
                assert_eq!(a, b);
                let y = { assert_ne!(a, b); 1 };
            }"#,
        );
        assert_eq!(c.panic_count, 11);
    }

    #[test]
    fn non_panicking_lookalikes_are_not_counted() {
        let c = fn_counts(
            r#"fn f() {
                a.unwrap_or(1);
                a.unwrap_or_default();
                a.unwrap_or_else(|| 2);
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
    fn panics_inside_semicolon_separated_macro_bodies_are_counted() {
        // `vec![x; n]` is not a comma-separated expression list, so a
        // comma-only parse would skip it; splitting on `;` must find both.
        let c = fn_counts(
            r#"fn f() {
                let v = vec![a.unwrap(); n.expect("len")];
                let w = vec![b.unwrap(), c.unwrap()];
                let m = matches!(d.unwrap(), Some(_));
            }"#,
        );
        assert_eq!(c.panic_count, 5);
    }

    #[test]
    fn top_level_commas_in_turbofish_and_closures_stay_intact() {
        // These commas sit at the macro body's top token level; a naive
        // split would tear the expressions apart and lose the panics.
        let c = fn_counts(
            r#"fn f() {
                assert!(parse::<A, B>(x).unwrap());
                println!("{}", (|a, b| a.unwrap() + b)(1, 2));
                let v = vec![parse::<A, B>(y).unwrap(); 3];
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
        assert_eq!(suppression(&attr).lints, 1);
        assert_eq!(suppression(&attr).bare_lints(), 1);
        let attr: syn::Attribute = syn::parse_quote!(#[allow]);
        assert_eq!(suppression(&attr).lints, 0);
    }

    #[test]
    fn bare_panics_are_the_unexplained_subset() {
        let c = fn_counts(
            r#"fn f() {
                a.unwrap();
                b.expect("");
                b.expect("   ");
                b.expect();
                panic!();
                panic!("");
                std::unreachable!();
                todo!("later");
                unimplemented!("never");
                assert!(x);
                assert!(x, "");
                assert_eq!(a, b);
                assert_ne!(a, b,);
            }"#,
        );
        assert_eq!((c.panic_count, c.bare_panic_count), (13, 13));
        let c = fn_counts(
            r#"fn f() {
                b.expect("config was validated at load");
                b.expect(MSG);
                b.expect(&format!("no {key}"));
                panic!("boom: {x}");
                unreachable!("checked above");
                assert!(x, "x must hold: {x}");
                assert!(x, MSG);
                assert_eq!(a, b, "a and b differ");
                assert_ne!(a, b, "{a} == {b}");
            }"#,
        );
        assert_eq!((c.panic_count, c.bare_panic_count), (9, 0));
    }

    #[test]
    fn bare_panics_inside_macro_args_are_counted() {
        let c = fn_counts(r#"fn f() { assert!(a.unwrap(), "msg"); let v = vec![b.unwrap(); 2]; }"#);
        assert_eq!((c.panic_count, c.bare_panic_count), (3, 2));
    }

    #[test]
    fn only_reasoned_expect_suppressions_are_justified() {
        let bare = |src: &str| {
            let c = fn_counts(src);
            (c.allow_count, c.bare_allow_count)
        };
        assert_eq!(
            bare(r#"#[expect(dead_code, reason = "wip")] fn f() {}"#),
            (1, 0)
        );
        assert_eq!(
            bare(
                r#"#[expect(a, b, reason = "both")] fn f() { #[expect(c, reason = "x")] let y = 1; }"#
            ),
            (3, 0)
        );
        assert_eq!(bare("#[expect(dead_code)] fn f() {}"), (1, 1));
        assert_eq!(
            bare(r#"#[expect(dead_code, reason = " ")] fn f() {}"#),
            (1, 1)
        );
        assert_eq!(
            bare(r#"#[expect(dead_code, note = "x")] fn f() {}"#),
            (1, 1)
        );
        assert_eq!(
            bare(r#"#[expect(dead_code, reason = 1)] fn f() {}"#),
            (1, 1)
        );
        assert_eq!(
            bare(r#"#[allow(dead_code, reason = "wip")] fn f() {}"#),
            (1, 1)
        );
        assert_eq!(bare("fn f() { #[allow(a, b)] let x = 1; }"), (2, 2));
        assert_eq!(bare("#[inline] fn f() {}"), (0, 0));
    }

    #[test]
    fn file_level_bare_allows_and_denied_warning_groups() {
        let c = file_counts(
            r#"
            #![allow(unused)]
            #![expect(clippy::module_name_repetitions, reason = "API names")]
            #![deny(warnings, missing_docs)]
            #![forbid(clippy::all, clippy::pedantic, unsafe_code)]
            #![cfg_attr(not(debug_assertions), deny(warnings))]
            #![cfg_attr(feature = "x", allow(unused))]
            #![cfg_attr(bad =)]
            #![warn(clippy::all)]
            #![deny = "x"]
            #![deny(=)]
            #[deny(clippy::nursery)]
            struct S;
            fn f() { #![deny(warnings)] }
            #[cfg(test)]
            mod tests { #![deny(warnings)] }
            "#,
        );
        assert_eq!(c.allow_count, 2);
        assert_eq!(c.bare_allow_count, 1);
        // warnings + clippy::all + clippy::pedantic + cfg_attr'd warnings +
        // the struct's clippy::nursery; not the fn body or the test module.
        assert_eq!(c.deny_warnings_count, 5);
        assert_eq!(file_counts("#![deny(unsafe_code)]").deny_warnings_count, 0);
        // A reason doesn't hide the denied group, bare or under cfg_attr.
        let reasoned = r#"
            #![deny(warnings, reason = "keep builds clean")]
            #![cfg_attr(ci, forbid(clippy::all, unsafe_code, reason = "ci"))]
        "#;
        assert_eq!(file_counts(reasoned).deny_warnings_count, 2);
        let nested = "#![cfg_attr(unix, cfg_attr(debug_assertions, deny(warnings)))]";
        assert_eq!(file_counts(nested).deny_warnings_count, 1);
    }

    #[test]
    fn only_non_test_modules_count_toward_file_allow() {
        assert_eq!(file_count("mod m { #![allow(unused)] }"), 1);
        assert_eq!(file_count("#[cfg(test)] mod t { #![allow(unused)] }"), 0);
    }
}
