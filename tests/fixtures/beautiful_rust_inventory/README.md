# BeautifulRust inventory fixture

`src/lib.rs` is copied verbatim from the deliberately messy module in
[BeautifulRust](https://github.com/morganlinton/BeautifulRust)'s
`refactor-inventory-module` eval:

<https://raw.githubusercontent.com/morganlinton/BeautifulRust/main/skills/rust-best-practices/evals/refactor-inventory-module/files/inventory.rs>

(fetched at upstream commit `2b2d53a66f6f36ab4b8d6a93070183d91e77fad4`).

BeautifulRust is MIT licensed, Copyright (c) 2026 Morgan Linton. The full
license text is in [`LICENSE`](LICENSE) in this directory and applies to
`src/lib.rs`.

`tests/cli.rs` (`beautiful_rust_inventory_*`) copies `src/lib.rs` into a
temp crate and asserts what the gate catches. The fixture is deliberately
not a cargo target: there is no `Cargo.toml` here (the test writes one into
its temp copy), so cargo build / clippy / fmt and this repo's own gate
(`rust_roots = ["src"]`) never see it. Keep it byte-for-byte as upstream
wrote it.
