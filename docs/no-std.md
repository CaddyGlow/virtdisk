# Portable readers in 0.3

`virtdisk` supports stable `no_std` with `alloc`. Disable default features:

```toml
virtdisk = { version = "0.3", default-features = false }
```

The initial target requires pointer and 64-bit atomics (`Arc` and shared budget
counters). `x86_64-unknown-none` is validated by the pinned Nix shell and CI.
A working allocator is required. This does not promise allocation-free operation
or support for targets lacking those atomics.

## Breaking API migration

Implement `ReadAt` with `virtdisk::io::Result`, not `std::io::Result`. Format
constructors, extent visitors, views, physical validation and parser policy return
these same portable types in every feature configuration. `ReadContext.container`
is now an optional diagnostic `String`; it neither grants path authority nor
identifies retained storage. UTF-16 filesystem provenance remains lossless.
Host native paths live in `HostSourceContext`; opaque adapter state is forwarded
through transparent reader wrappers. No filesystem discovery occurs without `std`.

Errors implement `core::error::Error`, retain typed causes through `get_ref()` and
`source()`, and distinguish corrupt metadata, unsupported profiles, bounds,
resource limits, allocation failure, interruption, missing identity and backend
failure. Parser refusals retain `ParserLimitExceeded` resource/limit/requested
fields. On host builds `host_error()` and `raw_os_error()` recover the original
OS failure through context layers. Conversion to `std::io::Error` maps portable
resource-limit and missing-identity errors to `Unsupported` while retaining the
portable error as the source. Callers discard output on every failed exact read.
An empty read at EOF succeeds; an empty read beyond EOF fails.

`std` remains enabled by default. It enables files, native identity/path discovery,
locks, writers, journals, graph operations, inspection and image operations.
`cli` enables `std`. Host recovery checks still use retained native provenance,
including QCOW2 pending-journal refusal and validated VHDX log overlays.

## Supplied storage and parent authority

Portable constructors accept immutable retained `Arc<dyn ReadAt>` storage.
Parsed parents retain their original cumulative budget. Constructors accepting
limits reject a different limits object rather than silently creating independent
counters or ignoring a tighter ceiling. QCOW2 accepts explicitly supplied raw or
parsed QCOW2 parents; VDI and VHDX
validate native UUID/linkage against supplied parsed parents. VMDK offers
`open_with_parent` and `open_descriptor_bound` with named `VmdkExtentBinding`s.
Descriptor extent names select bindings; they do not authorize opening files.
Bindings must be complete and unambiguous, and lengths, offsets, profiles and
sparse capacities are checked before logical reads become available.

Issue `SourceIdentity` using a globally distinct provider namespace and a stable
storage number. Transparent wrappers and aliases preserve the number; distinct
containers with equal bytes receive different numbers. Embedded containers use
checked `region` tokens. Keep identities consistent for the lifetime of retained
graphs; namespaces must not collide between independent providers. The provider
is trusted, and tokens cannot prove a dishonest provider's claims. Host sources
register retained `same-file` handles in a process registry with an entropy-issued
namespace. Path replacement checks remain separate.

Parent paths reject repeated container tokens and retain depth limits; shared
ancestors on independent paths remain valid. VMDK additionally rejects extents
sharing underlying storage with the descriptor, another extent, or retained
ancestor dependencies, even when their embedded regions differ. Missing reliable
identity refuses parented and descriptor construction. Single-source reads do
not require identity.

## Accounting and allocation strategy

Budgets remain cumulative across failed operations and deferred reads. Failed
charges leave their counter unchanged; live reservations release on drop.
Scratch reservations cover the operation's live buffers, including decoded
output until it is consumed. They use the existing live cache counter, which
also bounds temporary storage. Metadata/work counters never reset to simulate
cache hits.

Host QCOW2 retains its synchronized hash mapping cache. A miss charges 128 bytes
of metadata and retained storage before reading; a hit avoids another source
read but still validates mapping semantics. Portable QCOW2 uses immutable parsed
metadata and no shared mutable mapping cache: each entry lookup charges eight
metadata bytes, one format work item, and the actual source-read work. Repeated
portable lookups can exhaust cumulative limits sooner. The host cache entry
ceiling does not apply to entries that are never retained. Both modes preserve
format, ownership, depth and decompression ceilings.

Collection choices are explicit:

| Use | Collection and allocation contract |
| --- | --- |
| QCOW2 mapping cache | Host-only `HashMap`, mutex, conservative 128-byte entry reservation and bounded entry count |
| QCOW2 snapshot duplicate IDs | Fallibly reserved snapshot `Vec`; compare retained bounded IDs without a second identifier allocation; traversal work charged |
| QCOW2 refcount block ownership | Fallibly reserved sorted `Vec`; insertion/traversal work charged conservatively |
| QCOW2 ownership counts and scratch | Fallibly reserved `Vec` with metadata and live-storage accounting |
| QCOW2 authorized paths/host identities | Host-only bounded hash sets |
| VDI mapping/ownership indexes | Fallibly reserved ordered vectors with existing map/depth ceilings |
| VMDK physical ownership | `BTreeMap`, bounded grain count and conservative per-grain retained reservation; node allocation is infallible |
| VMDK portable bindings/dependencies | Bounded vectors and explicit underlying-storage alias checks; shared descriptor budget |
| VMDK authorized/used paths | Host-only ordered sets |
| VHDX region/metadata duplicate IDs | Ordered sets with conservative per-entry storage and traversal charges; node allocation is infallible |
| VHDX locator properties | Ordered map, bounded locator entry/string sizes; node allocation is infallible |

Stable B-tree insertion does not offer recoverable allocation failure. Those
collections require an allocator whose exhaustion behavior is acceptable to the
caller; recoverable vector reservations return `OutOfMemory`.

Raw DEFLATE uses direct `miniz_oxide` in both modes. It requires decoder completion
and exact cluster output, refuses truncation and expansion past the cluster, and
retains the existing sector-padding/trailing-input policy. Zstd uses a private MIT-licensed copy of `ms-compress-ruzstd` 0.9.1 with additional
bounds before literal, sequence and decoded-block growth. The upstream window
ceiling alone did not bound malformed match expansion before allocation. Checksum
hashing remains enabled; window size, declared size, exact output and checksums
are validated. Before constructing the decoder, reserve input, output, twice the
window, sixteen maximum blocks and 64 KiB for fixed entropy tables/state. This
conservative rule may refuse tighter live-storage budgets that previously passed.
Internal bounded decoder allocations remain infallible; input/output reservations
retain typed allocation errors. See [decoder provenance](../src/zstd_decoder/PROVENANCE.md).
`flate2` is only a host fixture-generation dev dependency.

## Validation and downstream rollout

Portable integration fixtures under `tests/portable_*` run with all features and
without defaults. They cover each format, both QCOW2 compression families,
accounting modes, explicit parents, identity/linkage errors, VMDK split bindings,
and VHDX recovery overlays. The independent consumer in
`integration/portable-consumer` compiles identical reader/policy/view code with
and without a second dependency enabling upstream `std`, and for bare metal.
Its bare-metal feature tree must have no host adapter dependencies. Original
VDI, VMDK, VHDX and QCOW2 corruption suites also retain their in-memory tests
without `std`; only their native process/path fixtures are gated.

Partmgr migration is validated using a temporary local patch, not a release
manifest sibling dependency. Publish 0.3 before switching partmgr's release
manifest to the registry version with disabled defaults and explicit `std`
feature wiring. Publication and downstream rollout are separate release steps.
Host Rust tests do not establish firmware boot, Windows servicing or capture
correctness; existing native validation gates remain separate.
