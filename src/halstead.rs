//! Per-function Halstead difficulty computed from a `syn` AST.
//!
//! **Halstead difficulty** — `(n1 / 2) * (N2 / n2)` over the token stream of
//! the function body, where keywords/punctuation are operators and
//! idents/literals are operands. (Cyclomatic and cognitive complexity come
//! from `cccc-rs`; see `rust_metrics`.)

use std::collections::HashSet;

use proc_macro2::{TokenStream, TokenTree};

pub fn halstead_difficulty(block: &syn::Block) -> f64 {
    use quote::ToTokens;
    let mut counts = HalsteadCounts::default();
    let stream = block.to_token_stream();
    count_tokens(stream, &mut counts);
    counts.difficulty()
}

#[derive(Default)]
struct HalsteadCounts {
    operators: HashSet<String>,
    operands: HashSet<String>,
    total_operands: u64,
}

impl HalsteadCounts {
    fn operator(&mut self, tok: String) {
        self.operators.insert(tok);
    }

    fn operand(&mut self, tok: String) {
        self.operands.insert(tok);
        self.total_operands += 1;
    }

    fn difficulty(&self) -> f64 {
        let n1 = self.operators.len() as f64;
        let n2 = self.operands.len() as f64;
        if n2 == 0.0 {
            return 0.0;
        }
        (n1 / 2.0) * (self.total_operands as f64 / n2)
    }
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "static", "struct", "trait", "type", "unsafe", "use", "where", "while", "yield",
];

fn count_tokens(stream: TokenStream, counts: &mut HalsteadCounts) {
    for tree in stream {
        match tree {
            TokenTree::Group(g) => {
                counts.operator(format!("{:?}", g.delimiter()));
                count_tokens(g.stream(), counts);
            }
            TokenTree::Ident(i) => {
                let s = i.to_string();
                if KEYWORDS.contains(&s.as_str()) {
                    counts.operator(s);
                } else {
                    counts.operand(s);
                }
            }
            TokenTree::Punct(p) => counts.operator(p.as_char().to_string()),
            TokenTree::Literal(l) => counts.operand(l.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(src: &str) -> syn::Block {
        syn::parse_str(&format!("{{ {src} }}")).unwrap()
    }

    #[test]
    fn halstead_difficulty_is_positive_for_real_code() {
        let b = block("let x = a + b; let y = x * x;");
        assert!(halstead_difficulty(&b) > 0.0);
    }

    #[test]
    fn halstead_empty_block_is_zero() {
        assert_eq!(halstead_difficulty(&block("")), 0.0);
    }

    #[test]
    fn halstead_exact_formula() {
        // Tokens of `{ let x = a + a; }`: operators {brace, let, =, +, ;}
        // => n1 = 5; operands {x, a} => n2 = 2, N2 = 3.
        // D = (5/2) * (3/2) = 3.75 exactly.
        assert_eq!(halstead_difficulty(&block("let x = a + a;")), 3.75);
    }
}
