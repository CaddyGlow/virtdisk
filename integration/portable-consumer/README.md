# Independent portable API consumer

This separate Cargo workspace uses the same `#![no_std]` source and portable
trait implementation in every configuration. The optional `host-unifier`
package enables virtdisk's `std` feature through a second dependency, exercising
Cargo feature unification without changing the consumer's source API.

```sh
cargo test --manifest-path integration/portable-consumer/Cargo.toml --locked
cargo test --manifest-path integration/portable-consumer/Cargo.toml --locked --features host-unified
cargo check --manifest-path integration/portable-consumer/Cargo.toml --locked --target x86_64-unknown-none
cargo tree --manifest-path integration/portable-consumer/Cargo.toml --locked --target x86_64-unknown-none -e features
```

`exercise` validates parser limits and charges, reservation release, a positional
in-memory reader, bounded views, extent traversal, typed read provenance, and a
small sparse VDI fixture. The bare-metal build omits the host-enabling dependency.
The consumer requires pointer and 64-bit atomics, as does the library.
