# Repository guidelines

Rust 2024 crate `virtdisk`. Bounded read-only raw and QCOW2 virtual disk readers.
Sibling checkouts required for path dependencies: none.
Use `cargo fmt -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and `cargo test --all-features --locked`.
Host tests do not establish Windows servicing or capture correctness.
