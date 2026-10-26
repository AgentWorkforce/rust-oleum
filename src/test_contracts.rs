//! Preserve existing test declarations while permitting additive tests.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{ensure, Result};
use quote::ToTokens;
use syn::visit::Visit;

pub struct Contracts {
    pub tests: BTreeMap<String, String>,
    pub gates: BTreeMap<String, String>,
}

pub fn collect(root: &Path, inputs: &BTreeMap<String, String>) -> Result<Contracts> {
    let mut contracts = BTreeMap::new();
    let mut gates = BTreeMap::new();
    for file in inputs.keys().filter(|p| p.ends_with(".rs")) {
        let ast = syn::parse_file(&std::fs::read_to_string(root.join(file))?)?;
        let mut visitor = Tests {
            scope: vec![file.clone()],
            attributes: Vec::new(),
            tests: Vec::new(),
            gates: BTreeMap::new(),
        };
        visitor.visit_file(&ast);
        for (id, hash) in visitor.tests {
            ensure!(
                contracts.insert(id.clone(), hash).is_none(),
                "ambiguous test identity {id}"
            );
        }
        for (id, mut attrs) in visitor.gates {
            attrs.sort();
            gates.insert(id, crate::snapshot::digest(attrs.join("\n").as_bytes()));
        }
    }
    Ok(Contracts {
        tests: contracts,
        gates,
    })
}

struct Tests {
    scope: Vec<String>,
    attributes: Vec<String>,
    tests: Vec<(String, String)>,
    gates: BTreeMap<String, Vec<String>>,
}

impl<'ast> Visit<'ast> for Tests {
    fn visit_file(&mut self, node: &'ast syn::File) {
        self.gates.entry(self.scope.join("::")).or_default();
        self.attributes.push(attributes(&node.attrs));
        syn::visit::visit_file(self, node);
        self.attributes.pop();
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.attributes.push(attributes(&node.attrs));
        syn::visit::visit_item_mod(self, node);
        self.attributes.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if node
            .attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
        {
            let id = self.scope.join("::");
            let content = format!("{}\n{}", self.attributes.join("\n"), node.to_token_stream());
            self.tests
                .push((id, crate::snapshot::digest(content.as_bytes())));
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        self.scope.push(item_name(node));
        self.gates.entry(self.scope.join("::")).or_default();
        syn::visit::visit_item(self, node);
        self.scope.pop();
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        if node.path().is_ident("cfg") || node.path().is_ident("cfg_attr") {
            self.gates
                .entry(self.scope.join("::"))
                .or_default()
                .push(node.to_token_stream().to_string());
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.scope.push(node.sig.ident.to_string());
        self.gates.entry(self.scope.join("::")).or_default();
        syn::visit::visit_impl_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.scope.push(node.sig.ident.to_string());
        self.gates.entry(self.scope.join("::")).or_default();
        syn::visit::visit_trait_item_fn(self, node);
        self.scope.pop();
    }
}

fn item_name(item: &syn::Item) -> String {
    match item {
        syn::Item::Fn(i) => i.sig.ident.to_string(),
        syn::Item::Mod(i) => i.ident.to_string(),
        syn::Item::Struct(i) => i.ident.to_string(),
        syn::Item::Enum(i) => i.ident.to_string(),
        syn::Item::Trait(i) => i.ident.to_string(),
        syn::Item::Const(i) => i.ident.to_string(),
        syn::Item::Static(i) => i.ident.to_string(),
        syn::Item::Type(i) => i.ident.to_string(),
        syn::Item::Impl(i) => format!(
            "impl {} {}",
            i.self_ty.to_token_stream(),
            i.trait_
                .as_ref()
                .map(|(_, p, _)| p.to_token_stream().to_string())
                .unwrap_or_default()
        ),
        _ => "other-items".into(),
    }
}

fn attributes(attrs: &[syn::Attribute]) -> String {
    attrs
        .iter()
        .map(|a| a.to_token_stream().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_are_normalized_and_parent_gates_are_protected() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = BTreeMap::from([("lib.rs".into(), "ignored".into())]);
        let path = dir.path().join("lib.rs");
        let source = "#[cfg(test)] mod t { #[tokio::test] async fn one() { assert!(true); } fn helper() {} }";
        std::fs::write(&path, source).unwrap();
        let before = collect(dir.path(), &inputs).unwrap().tests;
        assert_eq!(before.len(), 1);
        assert!(before.contains_key("lib.rs::t::one"));
        std::fs::write(&path, source.replace("async fn", "async\nfn")).unwrap();
        assert_eq!(before, collect(dir.path(), &inputs).unwrap().tests);
        std::fs::write(&path, source.replace("cfg(test)", "cfg(any())")).unwrap();
        assert_ne!(before, collect(dir.path(), &inputs).unwrap().tests);
        std::fs::write(&path, "#[test] fn x() {} #[test] fn x() {}").unwrap();
        assert!(collect(dir.path(), &inputs).is_err());
    }

    #[test]
    fn conditional_attributes_are_attached_to_their_own_items() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = BTreeMap::from([("lib.rs".into(), "unused".into())]);
        let source = "use std::fmt; struct S; enum E {} type Alias = S; const C: u8 = 0; static N: u8 = 0; trait T { fn f(); } impl T for S { fn f() {} }";
        let path = dir.path().join("lib.rs");
        std::fs::write(&path, source).unwrap();
        let before = collect(dir.path(), &inputs).unwrap();
        std::fs::write(
            &path,
            source.replace(
                "impl T for S { fn f()",
                "impl T for S { #[cfg(any())] fn f()",
            ),
        )
        .unwrap();
        let after = collect(dir.path(), &inputs).unwrap();
        assert_ne!(
            before.gates["lib.rs::impl S T::f"],
            after.gates["lib.rs::impl S T::f"]
        );
        assert_eq!(before.gates["lib.rs::T::f"], after.gates["lib.rs::T::f"]);
    }
}
