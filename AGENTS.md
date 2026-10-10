# Repository guidelines

Rust 2024 crate `virtdisk`. Bounded virtual disk readers, writers, and offline image operations for raw, QCOW2, VDI, VMDK, and VHDX images.
Read support and write support are limited to concrete format profiles; see the support matrix in `README.md` for format and platform restrictions.
Reader handles implement the immutable `ReadAt` contract; separate writer handles implement `WriteAt` and retain exclusive OS file locks. Callers must exclude external image access during writes and recovery; payload writes may partially complete on I/O failure.
Sibling checkouts required for path dependencies: none.
Use `cargo fmt -- --check`, `cargo clippy --all-targets --all-features --locked -- -D warnings`, and `cargo test --all-features --locked`.
Host tests do not establish Windows servicing or capture correctness.
