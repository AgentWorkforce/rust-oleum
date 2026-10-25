//! Public-API shape checks that clippy leaves alone, from the Rust API
//! Guidelines:
//!
//! - shared-ownership / interior-mutability wrappers (`Rc`, `RefCell`,
//!   `Arc`, `Box`, `Cell`, `Mutex`) in the parameter or return types of a
//!   `pub` fn leak an implementation choice into every caller (C-GENERIC,
//!   C-STRUCT-PRIVATE);
//! - `get_`-prefixed getters (C-GETTER);
//! - error types returned from `pub` fns that don't implement
//!   `std::error::Error` (C-GOOD-ERR).
//!
//! "Public" means declared `pub` (not `pub(crate)`); whether the enclosing
//! module is reachable from the crate root is not resolved. Methods in trait
//! impls never count: their signatures belong to the trait.
//!
//! The error-type check is crate-wide: types are matched by name across the
//! files of one source root, so it does not resolve paths or re-exports. A
//! type counts as implementing `Error` when the crate has an
//! `impl .. Error for Name` or the type derives `Error` (thiserror) or
//! `Snafu`. Error types the crate doesn't define (`std::io::Error`,
//! `anyhow::Error`, `String`) are out of scope.

use std::collections::{BTreeMap, BTreeSet};

use syn::visit::Visit;

/// Wrapper types that should stay out of public signatures.
const WRAPPER_TYPES: &[&str] = &["Rc", "RefCell", "Arc", "Box", "Cell", "Mutex"];

/// `get_` methods that the API Guidelines keep: they name what is gotten.
const GETTER_EXCEPTIONS: &[&str] = &["get_mut", "get_ref", "get_unchecked", "get_unchecked_mut"];

/// Derives that implement `std::error::Error`.
const ERROR_DERIVES: &[&str] = &["Error", "Snafu"];

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

/// Wrapper types named in a `pub` fn's parameter and return types (each
/// occurrence counts, nested ones too). `self: Arc<Self>` receivers are
/// left alone.
pub fn pub_wrapper_count(vis: &syn::Visibility, sig: &syn::Signature) -> u32 {
    if !is_pub(vis) {
        return 0;
    }
    let mut v = WrapperVisitor::default();
    for input in &sig.inputs {
        if let syn::FnArg::Typed(arg) = input {
            v.visit_type(&arg.ty);
        }
    }
    v.visit_return_type(&sig.output);
    v.count
}

#[derive(Default)]
struct WrapperVisitor {
    count: u32,
}

impl<'ast> Visit<'ast> for WrapperVisitor {
    fn visit_path_segment(&mut self, node: &'ast syn::PathSegment) {
        if WRAPPER_TYPES.iter().any(|w| node.ident == w) {
            self.count += 1;
        }
        syn::visit::visit_path_segment(self, node);
    }
}

/// 1 for a `pub fn get_x(&self)` (or `&mut self`) taking nothing else.
pub fn get_prefix_count(vis: &syn::Visibility, sig: &syn::Signature) -> u32 {
    let name = sig.ident.to_string();
    let mut inputs = sig.inputs.iter();
    let ref_receiver = matches!(
        inputs.next(),
        Some(syn::FnArg::Receiver(r)) if r.reference.is_some()
    );
    let getter = is_pub(vis)
        && ref_receiver
        && inputs.next().is_none()
        && name.len() > "get_".len()
        && name.starts_with("get_")
        && !GETTER_EXCEPTIONS.contains(&name.as_str());
    u32::from(getter)
}

/// Crate-wide scan for error types: feed every production file of one crate
/// to [`ErrorTypeScan::scan`], then [`ErrorTypeScan::finish`].
#[derive(Debug, Default)]
pub struct ErrorTypeScan {
    /// Type name -> files defining a struct / enum / union of that name.
    defined: BTreeMap<String, BTreeSet<String>>,
    /// Names with an `Error` impl or derive.
    implemented: BTreeSet<String>,
    /// Names used as `E` in a public `Result<_, E>`.
    returned: BTreeSet<String>,
}

impl ErrorTypeScan {
    pub fn scan(&mut self, file: &str, ast: &syn::File) {
        ErrorVisitor { file, scan: self }.visit_file(ast);
    }

    /// `"file::Type"` -> 1 when a public error type defined in the crate
    /// lacks an `Error` impl, 0 when it has one.
    pub fn finish(self) -> BTreeMap<String, u32> {
        let mut out = BTreeMap::new();
        for name in &self.returned {
            let missing = u32::from(!self.implemented.contains(name));
            for file in self.defined.get(name).into_iter().flatten() {
                out.insert(crate::config::function_key(file, name), missing);
            }
        }
        out
    }

    fn define(&mut self, file: &str, ident: &syn::Ident, attrs: &[syn::Attribute]) {
        let name = ident.to_string();
        if derives_error(attrs) {
            self.implemented.insert(name.clone());
        }
        self.defined
            .entry(name)
            .or_default()
            .insert(file.to_string());
    }

    /// Record `E` when `ty` is `Result<_, E>` (any path ending in `Result`).
    fn returns(&mut self, ty: &syn::Type) {
        if let Some(name) = result_error_name(ty) {
            self.returned.insert(name);
        }
    }

    fn returns_from(&mut self, vis: &syn::Visibility, sig: &syn::Signature) {
        if let (true, syn::ReturnType::Type(_, ty)) = (is_pub(vis), &sig.output) {
            self.returns(ty);
        }
    }
}

/// `#[derive(.., Error, ..)]` / `#[derive(thiserror::Error)]` / `Snafu`.
fn derives_error(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("derive"))
        .any(|a| {
            a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|paths| paths.iter().any(|p| last_ident_in(p, ERROR_DERIVES)))
        })
}

fn last_ident_in(path: &syn::Path, names: &[&str]) -> bool {
    path.segments
        .last()
        .is_some_and(|s| names.iter().any(|n| s.ident == n))
}

/// The error type's name in `Result<T, E>`, when `E` is a plain path.
fn result_error_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let last = p.path.segments.last()?;
    if last.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    let types: Vec<&syn::Type> = args
        .args
        .iter()
        .filter_map(|a| match a {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .collect();
    match types.get(1) {
        Some(syn::Type::Path(e)) => e.path.segments.last().map(|s| s.ident.to_string()),
        _ => None,
    }
}

struct ErrorVisitor<'a> {
    file: &'a str,
    scan: &'a mut ErrorTypeScan,
}

impl<'ast> Visit<'ast> for ErrorVisitor<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !crate::rust_metrics::is_cfg_test(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.scan.define(self.file, &node.ident, &node.attrs);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.scan.define(self.file, &node.ident, &node.attrs);
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        self.scan.define(self.file, &node.ident, &node.attrs);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if crate::rust_metrics::is_cfg_test(&node.attrs) {
            return;
        }
        if let Some((_, path, _)) = &node.trait_ {
            if last_ident_in(path, &["Error"]) {
                let name = crate::rust_metrics::type_name_of(&node.self_ty);
                self.scan.implemented.insert(name);
            }
        }
        syn::visit::visit_item_impl(self, node);
    }

    /// `pub type Result<T> = std::result::Result<T, MyError>;` exposes
    /// `MyError` as surely as a fn returning it.
    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if is_pub(&node.vis) {
            self.scan.returns(&node.ty);
        }
    }

    // Signatures only: types and impls inside fn bodies aren't public API.
    // Test-only fns (`#[test]`, `#[cfg(test)]`) aren't public API either.
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !crate::rust_metrics::has_test_attr(&node.attrs) {
            self.scan.returns_from(&node.vis, &node.sig);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !crate::rust_metrics::has_test_attr(&node.attrs) {
            self.scan.returns_from(&node.vis, &node.sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fn_item(src: &str) -> syn::ItemFn {
        syn::parse_str(src).unwrap()
    }

    fn wrappers(src: &str) -> u32 {
        let f = fn_item(src);
        pub_wrapper_count(&f.vis, &f.sig)
    }

    fn method(src: &str) -> syn::ImplItemFn {
        syn::parse_str(src).unwrap()
    }

    fn getter(src: &str) -> u32 {
        let m = method(src);
        get_prefix_count(&m.vis, &m.sig)
    }

    fn errors(files: &[(&str, &str)]) -> BTreeMap<String, u32> {
        let mut scan = ErrorTypeScan::default();
        for (file, src) in files {
            scan.scan(file, &syn::parse_file(src).unwrap());
        }
        scan.finish()
    }

    #[test]
    fn wrappers_in_pub_signatures_are_counted() {
        assert_eq!(
            wrappers("pub fn f(a: Rc<A>, b: &RefCell<B>) -> Arc<C> {}"),
            3
        );
        assert_eq!(wrappers("pub fn f(a: Vec<Box<dyn Fn()>>) {}"), 1);
        assert_eq!(
            wrappers("pub fn f(a: std::sync::Arc<std::sync::Mutex<u8>>) {}"),
            2
        );
        assert_eq!(wrappers("pub fn f(c: Cell<u8>) -> Option<Rc<u8>> {}"), 2);
        // Only the signature counts, not the body.
        assert_eq!(
            wrappers("pub fn f(x: u8) -> u8 { let b = Box::new(x); *b }"),
            0
        );
    }

    #[test]
    fn non_pub_fns_and_receivers_are_left_alone() {
        assert_eq!(wrappers("fn f(a: Rc<A>) -> Box<B> {}"), 0);
        assert_eq!(wrappers("pub(crate) fn f(a: Rc<A>) {}"), 0);
        let m = method("pub fn f(self: Arc<Self>, other: Arc<Self>) {}");
        assert_eq!(pub_wrapper_count(&m.vis, &m.sig), 1);
        // Lookalike names don't count.
        assert_eq!(wrappers("pub fn f(a: Boxed, b: RcStr, c: Cellar) {}"), 0);
    }

    #[test]
    fn get_prefixed_getters_are_flagged() {
        assert_eq!(getter("pub fn get_name(&self) -> &str {}"), 1);
        assert_eq!(getter("pub fn get_buf(&mut self) -> &mut Vec<u8> {}"), 1);
    }

    #[test]
    fn non_getters_and_sanctioned_names_are_not() {
        for src in [
            "pub fn name(&self) -> &str {}",
            "fn get_name(&self) -> &str {}",
            "pub(crate) fn get_name(&self) -> &str {}",
            "pub fn get_item(&self, i: usize) -> u8 {}",
            "pub fn get_name(self) -> String {}",
            "pub fn get_default() -> u8 {}",
            "pub fn get(&self) -> u8 {}",
            "pub fn get_(&self) -> u8 {}",
            "pub fn get_mut(&mut self) -> &mut u8 {}",
            "pub fn get_ref(&self) -> &u8 {}",
            "pub fn get_unchecked(&self) -> u8 {}",
            "pub fn get_unchecked_mut(&mut self) -> &mut u8 {}",
            "pub fn getter(&self) -> u8 {}",
        ] {
            assert_eq!(getter(src), 0, "{src}");
        }
    }

    #[test]
    fn public_error_types_without_error_impl_are_flagged() {
        let found = errors(&[
            (
                "src/a.rs",
                "pub struct ParseError; pub enum IoFail { A } pub struct Fine;\n\
                 pub fn parse() -> Result<u8, ParseError> {}\n\
                 pub fn read() -> std::result::Result<(), IoFail> {}\n\
                 pub fn ok() -> Result<(), Fine> {}",
            ),
            ("src/b.rs", "impl std::error::Error for Fine {}"),
        ]);
        assert_eq!(
            found,
            BTreeMap::from([
                ("src/a.rs::Fine".to_string(), 0),
                ("src/a.rs::IoFail".to_string(), 1),
                ("src/a.rs::ParseError".to_string(), 1),
            ])
        );
    }

    #[test]
    fn error_impls_derives_and_aliases_are_recognised() {
        let found = errors(&[(
            "src/e.rs",
            "#[derive(Debug, thiserror::Error)] pub enum A {}\n\
             #[derive(Debug, Snafu)] pub struct B;\n\
             #[derive(Debug, Clone)] pub struct C;\n\
             pub union D { x: u8 }\n\
             impl Error for D {}\n\
             pub type Result<T> = std::result::Result<T, C>;\n\
             pub fn a() -> Result<(), A> {}\n\
             pub fn b() -> Result<(), B> {}\n\
             pub fn d() -> Result<(), D> {}\n\
             pub struct F; pub struct G;\n\
             mod inner { pub fn f() -> Result<(), F> {} }\n\
             pub fn g() -> Result<'static, (), G> {}\n\
             pub fn bare() -> Result {}\n\
             pub fn by_ref() -> &'static str {}",
        )]);
        assert_eq!(
            found,
            BTreeMap::from([
                ("src/e.rs::A".to_string(), 0),
                ("src/e.rs::B".to_string(), 0),
                ("src/e.rs::C".to_string(), 1),
                ("src/e.rs::D".to_string(), 0),
                ("src/e.rs::F".to_string(), 1),
                ("src/e.rs::G".to_string(), 1),
            ])
        );
    }

    #[test]
    fn private_foreign_and_test_error_types_are_ignored() {
        let found = errors(&[(
            "src/p.rs",
            "struct Local; pub struct Hidden; pub struct Other;\n\
             fn private() -> Result<(), Local> {}\n\
             pub(crate) fn crate_only() -> Result<(), Hidden> {}\n\
             type Alias = Result<(), Hidden>;\n\
             pub fn io() -> Result<(), std::io::Error> {}\n\
             pub fn boxed() -> Result<(), Box<dyn std::error::Error>> {}\n\
             pub fn one_arg() -> Result<()> {}\n\
             pub fn tuple() -> Result<(), (Other,)> {}\n\
             pub fn not_result() -> Option<Other> {}\n\
             pub fn unit() {}\n\
             impl S { pub fn m(&self) -> Result<(), Other> { struct Inner; } }\n\
             #[cfg(test)] mod tests { pub struct T; pub fn t() -> Result<(), T> {} }\n\
             #[cfg(test)] impl std::error::Error for Other {}\n\
             pub struct TestOnly;\n\
             #[cfg(test)] pub fn helper() -> Result<(), TestOnly> {}\n\
             #[test] pub fn case() -> Result<(), TestOnly> {}\n\
             impl S { #[cfg(test)] pub fn h(&self) -> Result<(), TestOnly> {} }",
        )]);
        assert_eq!(found, BTreeMap::from([("src/p.rs::Other".to_string(), 1)]));
    }
}
