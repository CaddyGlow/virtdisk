# Virtual disk read/write and image management plan

Status: delivery plan. Individual entries describe intended support, not a
claim that every capability is implemented. Track verified implementation and
remaining release gates in [implementation-status.md](implementation-status.md).

## How to use this plan

This file is the detailed design and delivery checklist for the requested
formats and management operations. It does not authorize changing guest
partitions or filesystems when resizing a container.

For each implementation increment, first select a format profile and operation,
write a failing behavioral test, implement the smallest complete change, and
verify normal operation, interruption recovery, and independent interoperability.
Record the resulting supported profile and evidence in the status ledger.
Keep planned capabilities separate from capabilities that have passed these
gates; a broad format label alone is not a completion criterion.

## Objective

Extend `virtdisk` from bounded raw/QCOW2 readers into a native Rust library for
offline virtual disk inspection, read/write access, creation, conversion,
capacity changes, derived images, disk snapshots, and storage reclamation.
Provide an optional CLI built on the library. Preserve existing read-only
callers and parser limits.

Requested formats are raw, QCOW2, VHDX, VMware VMDK, and VirtualBox VDI.
Support is defined by an explicit format profile and operation, rather than
by extension or a single supported/unsupported flag.

## Starting baseline at planning time

This section records the original starting point. It is not the current
capability inventory; use the implementation status document for that.

- Rust 2024 crate with Rust 1.99 MSRV and development toolchain.
- `ReadAt` provides bounded exact positional reads and a fixed logical length.
- `RawDisk` opens regular files read-only; `DiskView` retains bounded parents.
- QCOW2 v2/v3 standard mappings, authorized raw/QCOW2 backing chains,
  deflate/zstd decompression, and active allocation/refcount validation exist.
- QCOW2 internal snapshots, encryption, extended L2, external data files,
  persistent bitmaps, and dirty/corrupt images are currently rejected.
- Parser budgets cover metadata, caches, work, recursion, and decoding.
- The separate `fuzz/` workspace already uses honggfuzz 0.5.62 and supports
  deterministic replay and bounded campaigns.
- Partition and filesystem interpretation belongs to higher-level components.
  Disk container operations must not silently rewrite guest filesystems.

## Scope and boundaries

Initial operation is offline on regular image files. Live VM writes, physical
device access, network storage, VM configuration editing, memory snapshots,
guest quiescing, and application-consistent multi-disk snapshots are separate
integrations. Their absence must be explicit in documentation.

Disk growth changes container capacity. Disk shrink removes addressable tail
bytes. Compaction reduces host allocation without reducing capacity. Guest
partition/filesystem resizing is a separate operation requiring its own
validation and implementation.

Legacy VHD, encryption, uncommon VMDK profiles, and persistent incremental
backup tracking remain later milestones. Do not advertise them before their
profiles and validation gates are implemented.

## Architecture and public contracts

### Reader compatibility

Keep the existing `ReadAt` contract and constructors compatible. Existing
readers retain a fixed logical length. Resizing requires exclusive ownership
and reopening readers and views; an active reader must not silently acquire a
different length or stale mapping.

### Proposed interfaces

Names below are design candidates to finalize in milestone 1:

- `ImageFormat` and format-specific profile identifiers.
- `OpenOptions`: explicit read-only/read-write mode, expected format, parent
  and extent authorization, recovery policy, and resource limits.
- `ImageInfo`: virtual size, physical file size and allocated storage when
  available, sector sizes, allocation unit, identifiers, features, parent
  references, snapshots, recovery state, and validation status.
- `ImageCapabilities`: operation availability, constraints, and reasons an
  operation is unavailable for this particular image and opening mode.
- A writer interface with positional writes, write-zeroes, discard, and
  explicit durable flush. Reads from a writable handle need separately
  documented synchronization; they do not inherit immutable-reader semantics.
- A management interface for creation, resize, snapshots, and chain changes.
  Separate optional capabilities rather than requiring every format to
  implement every operation.
- A streaming extent-map API distinguishing allocated data, explicit zero,
  unallocated ranges, and inherited data. Bound memory independently of disk
  size; avoid collecting every extent into a vector.
- Operation contexts with progress, cancellation, resource accounting, and
  typed errors preserving I/O sources and container/range provenance.

Separate logical block access, host storage primitives, format implementations,
and generic image operations. Keep CLI parsing out of the library.

### Detection and dependencies

Detect known container signatures with bounded reads. Require explicit format
selection for creation. Treat raw as an explicit choice or a documented
fallback; malformed recognized containers must not fall back to raw. Text VMDK
descriptors require bounded parsing and may reference multiple files.

Keep format implementations native Rust. Independent tools are validation
oracles, not hidden runtime dependencies. If an external-tool adapter is later
added, expose it explicitly as a separate capability.

### Concurrency and authorization

- Default to read-only; opening for mutation is explicit.
- Use one writer per image, with platform-specific locking behavior documented
  and tested. Advisory locks cannot guarantee cooperation from hypervisors.
- Serialize metadata mutations and define ordering between reads, writes,
  flushes, and management operations.
- Authorize embedded parent and extent references explicitly. Check canonical
  paths and opened-file identities, cycles, depth, unexpected protocols, and
  aliases. Do not rely on path strings alone.
- Parents of derived images remain immutable. Changing a parent requires
  explicit chain management and checks for known dependents.
- A standalone library cannot discover every child stored elsewhere. Require
  a caller-supplied dependency graph or ownership declaration for destructive
  chain operations; document this limit.

## Write, durability, and recovery rules

Before shipping each writer, document its update protocol and test it:

1. Validate writable profile, affected ranges, metadata ownership, and limits.
2. Reserve required storage and prepare allocation metadata.
3. Write data and persist prerequisites before publishing new mappings.
4. Update mappings/refcounts/bitmaps in a recoverable order.
5. Persist the commit point and release obsolete allocations safely.
6. Reopen and validate recovery behavior after interruption at each step.

The exact sequence is format-specific. Do not claim atomic multi-sector data
writes merely because metadata changes are recoverable. Define partial writes,
ambiguous I/O failures, poisoned handles, and required reopen/recovery behavior.

VHDX needs native log processing and transaction semantics. QCOW2 needs a
documented dirty-state and recovery strategy; its dirty flag alone is not a
transaction journal. For VDI/VMDK profiles without an adequate native recovery
mechanism, evaluate copy-to-new-image operations or explicit sidecar recovery.
Sidecars require identity checks, lifecycle rules, and documented portability
limits; choose the mechanism before advertising crash recovery.

Conversion, flattening, and initial compaction produce temporary outputs,
validate them, persist them, and publish only on success. Account for directory
durability and platform-specific rename behavior. Multi-file VMDK publication
needs a separate protocol; a single rename does not atomically publish a set.

Cancellation must have documented boundaries: disposable outputs may be
abandoned, but cancellation cannot leave an in-place metadata transaction
half-published. Recovery must distinguish process termination, simulated I/O
failure, and actual power-loss evidence.

## Format profiles

| Format | Initial reader profile | Initial writer profile | Later work |
| --- | --- | --- | --- |
| Raw | Existing regular-file reader | Bounded writes, zeroing, sparse/fixed creation, flush, resize | Preallocation, host hole punching and optimized copies |
| QCOW2 | Preserve v2/v3 support and existing validation | Standard v3, standalone and external-overlay images, allocation/refcounts, zero/discard | Internal snapshots, extended L2, persistent bitmaps, compressed export, external data |
| VDI | Fixed/dynamic block maps, identifiers and structural validation | Fixed/dynamic creation and writes | Differencing chains, native resize and compaction |
| VHDX | Fixed/dynamic headers, regions, metadata, BAT, sector bitmaps and recovery | Fixed/dynamic creation and native logged updates | Differencing images and chain management |
| VMDK | Bounded descriptors, flat and hosted sparse extents, split files and parents | Selected hosted flat/sparse profiles | Stream-optimized import/export and additional explicitly named profiles |

### QCOW2

Preserve independent validation of existing compressed reads. Start writes
with ordinary uncompressed clusters. Writing to supported compressed inputs
may replace affected clusters with ordinary allocated clusters after correct
copy-on-write/refcount updates; compressed output is a separate capability.

Test shared tables/data, partial cluster updates, explicit-zero masking,
refcount overflow and growth, allocation reuse, final-cluster boundaries,
and reopened mappings. Restrict writable refcount widths/features initially
and report restrictions instead of silently converting metadata in place.

Internal snapshot validation must account for all snapshot-owned metadata and
data, not just the current active mapping. Enable internal snapshots only
after ownership reconstruction and lifecycle operations pass independent tests.

### VHDX

Validate redundant headers and region tables, checksums, sequence selection,
mandatory metadata, BAT states, logical/physical sector sizes, and sector
bitmap coverage. Bound log parsing and replay.

Microsoft requires replay before normal I/O when the log is nonempty. Reject
such images until recovery exists. A read-only recovery mode must construct
the recovered view without modifying the source, or refuse opening; it must
not silently ignore the log. Validate its behavior against native recovery.

Differencing support must validate parent identifiers and partial-block
inheritance. Allocation, payload writes, sector bitmaps, metadata logging,
and header transitions each need interruption tests.

### VMDK

Maintain a matrix of extent types and descriptor profiles. Validate declared
capacity against extents, grain tables/directories, redundant metadata where
applicable, descriptor identifiers, parent relationships, and split-file
boundaries. Authorize every referenced file.

Initially reject physical-device extents and unsupported ESXi-specific
profiles. Stream-optimized images are a distinct import/export profile;
do not promise ordinary random in-place writes to them.

### VDI

Validate headers, versions, block sizes, block-map entries, allocation counts,
offsets, identifiers, and parent relationships. Handle free/zero entries
according to the supported version. Distinguish ordinary images from
differencing images and preserve required identity relationships.

Test interoperability with VirtualBox; UUID policy for cloning and derivation
must avoid accidentally registering independent images with the same identity.

## Image management operations

| Operation | Contract and key constraints |
| --- | --- |
| Create | Format/profile, capacity, sector/block sizes and allocation policy; never overwrite implicitly |
| Info | Human and JSON output; report unsupported features and validation scope |
| Map | Stream logical allocation and inheritance information within budgets |
| Check | Read-only structural, ownership, chain and optional payload validation |
| Read/write | Checked offsets, explicit mode, documented partial I/O and durable flush |
| Zero | Subsequent reads return zero, including when a parent contains data |
| Discard/trim | Explicit read-after-discard policy; report alignment and reclamation limits |
| Grow | New logical range has documented content; validate metadata capacity and chain constraints |
| Shrink | Explicit tail-removal policy; no inferred filesystem safety; initially prefer new-output rewrite |
| Convert | Supported reader to supported writer; preserve logical bytes and sector geometry policies |
| Clone | Independent content copy, explicit new-identity and metadata-copy policy |
| Derive | Child over authorized immutable parent; native parent constraints remain enforced |
| Compare/hash | Logical-content verification independent of container representation |
| Compact | Reduce physical allocation while preserving capacity and logical content |
| Snapshot | List/create/open/revert/delete disk states where supported |
| Flatten | Materialize selected chain state into an independent image |
| Rebase | Preserve logical content when switching parent; unsafe metadata-only change is not default |
| Merge/commit | Explicit destination and branch checks; initially materialize a new output |
| Repair | Diagnose first; later introduce individually specified, recoverable repairs |
| Backup/export | Bounded streaming initially; persistent change tracking is a later capability |

Do not expose parent data when zeroing or trimming an overlay. Distinguish
deallocation from zero masking. Compaction must not discard nonzero inherited
data or assume guest free space is recognizable from container allocation.

Shrink reports the removed range and any mapped data/snapshot/chain impacts.
An optional zero-tail policy checks actual logical content, but does not certify
partition/filesystem safety. Reject unsafe native chain capacity changes until
their semantics are established.

A QCOW2 overlay over another readable format can eventually provide a generic
derived-image mechanism, but cross-format parents require explicit resolver
support and an interoperability profile. Do not imply every native format can
directly use every other format as its parent.

Snapshot deletion and merge must preserve remaining branches. A library-owned
snapshot manifest, if introduced, is separate from hypervisor VM metadata.
Initially manage external chain files through explicit caller relationships;
do not edit `.vbox`, VMware VM configuration, or Hyper-V registrations.

## Optional CLI

Build an optional binary over the same library contracts. Candidate commands:

```text
virtdisk info IMAGE --json
virtdisk check IMAGE
virtdisk map IMAGE --json
virtdisk create OUTPUT --format FORMAT --size SIZE
virtdisk convert INPUT OUTPUT --format FORMAT
virtdisk clone INPUT OUTPUT
virtdisk derive PARENT CHILD --format FORMAT
virtdisk resize IMAGE --size SIZE
virtdisk compact INPUT OUTPUT
virtdisk compare LEFT RIGHT
virtdisk hash IMAGE
virtdisk zero IMAGE --offset OFFSET --length LENGTH
virtdisk trim IMAGE --offset OFFSET --length LENGTH
virtdisk snapshot list|create|open|revert|delete ...
virtdisk flatten INPUT OUTPUT
virtdisk rebase ...
virtdisk merge ...
```

Finalize syntax with the API. Require explicit destructive options for shrink,
overwrite, and in-place chain changes. Provide planning reports for management
operations, progress/cancellation for long work, and stable machine-readable
errors. Clearly identify whether an operation is native or produces a new file.

## Delivery milestones and completion gates

Deliver the milestones in the order below, with independently reviewable
changes for each format profile and operation. The API/raw foundation comes
first, followed by QCOW2, VDI, VHDX, and VMDK. Advanced management follows once
the relevant writers, ownership checks, and recovery contracts pass their
gates. Reader and fixture work for another format can proceed independently;
mutation must wait for its validation and durability prerequisites.

For every delivered capability, record the exact format/version/profile,
supported platforms, alignment and capacity limits, dependency authorization,
failure behavior, tests, and native-tool evidence in the status document.
Keep unsupported cases explicit. A milestone is complete only when its
documented public behavior and release gates are satisfied.

### Test-driven implementation workflow

For each capability, first write a failing behavioral test that expresses its
public contract. Confirm that it fails for the missing behavior, implement the
smallest complete change, then refactor while keeping the test green. Record
the failing case and resulting verification in the implementation status ledger.

Use bounded byte models for read/write and management sequences. For metadata
mutations, add interruption tests at each persistence boundary before enabling
the operation: reopen through the public recovery path, check logical content,
validate allocation ownership, and verify parent and sibling preservation.

Keep independently generated fixtures and oracle checks separate from tests
that exercise our own exporters. A successful round trip through our reader
and writer alone does not establish interoperability. Promote minimized fuzz
findings into deterministic regression tests before fixing their causes.

Each milestone is complete only when its stated acceptance gates pass. Track
implemented profiles, deferred operations, and unavailable native runtime
validation explicitly in `docs/implementation-status.md`.

### 1. API foundation and inspection

- Write operation contracts and format capability matrix.
- Introduce inspection, explicit opening modes, typed operation errors,
  streaming extent maps, and storage abstractions.
- Preserve public read APIs and bounded behavior.
- Document locking, recovery, resize, and parent-resolution rules.

Gate: current tests continue passing; supported/unsupported profiles and
inspection results have fixture coverage; no mutation is implied by opening.

### 2. Raw writer and generic operations

- Implement creation, bounded writes, zeroing, durable flush, and capacity
  changes under exclusive access.
- Implement logical compare/hash and bounded conversion infrastructure.
- Define host allocation reporting and discard fallbacks.
- Add initial CLI inspection and creation/conversion commands if CLI work is
  included in the release.

Gate: reopen and compare against reference bytes; boundary, disk-full,
cancellation, and partial-I/O scenarios pass on Linux and Windows.

### 3. QCOW2 writer and external overlays

- Implement the declared standard v3 writable profile.
- Add copy-on-write allocation, refcount updates, flush and recovery protocol.
- Add derived images, zero masking, discard, growth, and flattening.
- Use new-output paths for complex shrink/compaction/merge initially.

Gate: QEMU reads created/mutated images; logical content agrees with the model;
independent consistency checks pass; injected interruption recovers according
to the documented contract.

### 4. VDI fixed/dynamic support

- Add bounded readers and validation, then creation/writes.
- Introduce correct clone identities and allocation semantics.
- Add differencing support only after parent identity and inheritance tests.

Gate: VirtualBox-created fixtures read correctly and VirtualBox accepts outputs;
write/reopen and interrupted metadata updates pass the defined recovery model.

### 5. VHDX support

- Implement validated clean readers and log parsing/recovery first.
- Implement fixed/dynamic writers with native logging and header updates.
- Add differencing chains after sector bitmap and parent validation.

Gate: native Windows/Hyper-V tooling validates outputs and recovered fixtures;
512/4096-byte logical sectors, partial blocks, redundant metadata, and log
interruption cases pass.

### 6. VMDK hosted profiles

- Add descriptors, flat/sparse readers, split extents and parents.
- Implement explicitly selected writable profiles and multi-file publication.
- Add stream-optimized import/export after ordinary profiles are stable.

Gate: VMware and independent tools agree on logical content and supported
metadata; split-file, parent, identifier, and interruption scenarios pass.

### 7. Advanced management and optimization

- Complete snapshot lifecycle, branch-aware rebase/merge and compaction.
- Add QCOW2 internal snapshots, followed by additional feature profiles.
- Introduce constrained repair operations and incremental backup tracking.
- Benchmark metadata caches, sequential/random I/O, conversion memory use,
  storage amplification, and recovery overhead before optimization.

Gate: every operation has a capability entry, failure contract, independent
interoperability evidence, and applicable fuzz/fault-injection coverage.

Deliver profiles incrementally. Do not wait for every advanced feature before
releasing independently validated readers and writers.

## Honggfuzz strategy

Use honggfuzz, following `../vlmcsd-rs/fuzz` conventions and extending the
existing independent `virtdisk/fuzz` package. Do not switch to libFuzzer.

- Keep fuzz dependencies out of production builds and track `fuzz/Cargo.lock`.
- Keep reusable harness functions in `fuzz/src/lib.rs`, with thin honggfuzz
  binaries and deterministic replay using the identical functions.
- Preserve the existing pinned honggfuzz dependency and matching runner version.
- Use stable Rust in `.#fuzz`; retain `.#nightly` for optional experiments.
- Keep overflow checks and debug information enabled in fuzz release builds.
- Use independent target/workspace directories, locked builds, disabled
  incremental compilation, and explicitly configured compiler/wrapper settings
  where required by the instrumentation workflow.

Planned targets:

1. Raw bounds and management operations.
2. QCOW2, VHDX, VMDK descriptor/extent, and VDI parsing and logical reads.
3. Authorized parent/extent graph resolution and identity/cycle checks.
4. Stateful write, zero, discard, flush, reopen, grow, and shrink sequences.
5. Snapshot/derive/rebase/merge sequences and branch preservation.
6. Deterministic fault injection into writes, flushes, allocation and publication.
7. Conversion and logical comparison across supported profiles.

Combine arbitrary malformed input with mutation of valid structural seeds.
Stateful targets operate on small logical disks and compare against a bounded
reference byte model. Inject failures at deterministic operation counts and
reopen outputs through the normal implementation to exercise recovery.

No input-supplied host path is opened. Resolve paths through a controlled fake
resolver or a harness-owned temporary directory. Bound input size, disk size,
metadata, reads/writes, decompression, chain depth, allocations and execution
time. A timeout is a finding only under a reproducible, documented budget.

Retain seed hashes, source revision, tool versions, environment settings, raw
logs, campaign counts, minimized findings, and replay commands. Convert fixed
findings into regression fixtures. Use bounded CI smoke campaigns and sustained
campaigns separately; neither is proof of absence of bugs.

Independent-tool comparisons belong in controlled integration campaigns rather
than spawning a hypervisor/tool for every honggfuzz input. Sanitizer campaigns
may supplement honggfuzz where toolchain/platform support is established.

## Verification and environment

The Nix flake provides Linux x86_64/aarch64 shells, stable Rust 1.99.0,
Windows MSVC target libraries and cargo-xwin, native honggfuzz dependencies,
and a separate nightly shell. Use stable through the default shell and
`.#fuzz`; retain nightly under `.#nightly` for experiments that require it.
Follow the sibling `../vlmcsd-rs/fuzz` Honggfuzz workspace pattern rather than
introducing a second fuzzing framework. Preserve existing toolchain choices.

The checkout includes a tracked `flake.lock` pinning Nix inputs. Preserve those
pins during validation; update them deliberately as environment work. Campaign
receipts must hash the lockfile as well as the flake and Cargo lockfiles.

Required Rust checks after implementation changes:

```sh
nix develop . --command cargo fmt -- --check
nix develop . --command cargo clippy --all-targets --all-features --locked -- -D warnings
nix develop . --command cargo test --all-features --locked
```

Fuzz workspace checks:

```sh
nix develop .#fuzz --command cargo fmt --manifest-path fuzz/Cargo.toml -- --check
nix develop .#fuzz --command cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
nix develop .#fuzz --command cargo test --manifest-path fuzz/Cargo.toml --locked
```

Use the repository campaign runner for bounded honggfuzz runs. Extend it for
new targets and keep deterministic replay available without instrumentation.
Evaluate changed shells and check Nix formatting after flake changes.

Independent validation should include:

- QEMU-created fixtures and checks for supported profiles.
- Native VirtualBox, VMware and Windows/Hyper-V acceptance for their formats.
- Randomized boundary/operation sequences and logical-content comparisons.
- Reopen checks, corruption fixtures and failure injection at persistence steps.
- Windows runtime tests in addition to cross-compilation.

Host tests and cross-builds do not establish Windows servicing, capture,
installation, VM boot, or actual power-loss correctness. Record each type of
evidence separately and scope support claims accordingly.

## Decisions to settle during implementation

- Final trait/interface names and whether the CLI belongs in this package or
  a separate workspace package.
- Supported writable feature subsets for each format and version.
- Per-format transaction/recovery approach, including sidecar policy.
- Safe default behavior for discard, shrink, repair and parent modifications.
- Whether external snapshot metadata remains caller-owned or gains a manifest.
- Availability of native hypervisor test environments and fixture licensing.
- Encryption key handling, legacy VHD, and additional VMDK profiles as separate
  extensions after core write and recovery support.

These decisions should be resolved in concrete milestone designs; none should
weaken existing reader bounds or turn an unsupported profile into guessed I/O.

## Detailed execution checklist

Use this checklist with the milestones above. For each item, record the supported
profile, failing behavioral test, implementation, recovery evidence, independent
oracle result, and remaining limitations in the status ledger. Existing code
must be audited against these contracts rather than rewritten merely to follow
the sequence.

### Opening, inspection, and writable access

- Consolidate explicit standalone and authorized-chain opening for all five
  families; keep read-only opening the default and raw selection explicit.
- Provide a common writable handle that retains the concrete writer's lock and
  recovery state. Cover partial reads/writes, out-of-range access, flush, and
  lock release across every supported profile.
- Report format/version/profile, virtual and container sizes, sector geometry,
  allocation units, identity, parents, recovery requirements, and operation
  restrictions. Distinguish unavailable information from a measured zero.
- Test malformed recognized signatures, unsupported features, file replacement,
  aliases, denied parents, invalid extents, and aggregate chain budgets.

### Creation, conversion, and capacity

- Define fixed, sparse, and preallocated policies per format. Validate geometry
  and metadata bounds before creating output; never overwrite implicitly.
- Test empty images, final partial blocks, allocation limits, and independent
  acceptance of newly created fixed and dynamic images.
- Complete bounded conversion between each supported reader and exporter,
  preserving logical content while issuing fresh independent-image identities.
- Implement new-output growth and shrink first; require an explicit shrink
  policy and report the removed logical range. Verify zero-filled growth and
  optional zero-tail checking without asserting guest filesystem safety.
- Add native capacity changes individually after ownership, parent, snapshot,
  allocation-table relocation, interruption, and reopen tests pass.

### Derivation and external snapshots

- Implement native QCOW2, VDI, VMDK, and VHDX children over authorized parents.
  Enforce each format's identifier, geometry, and parent reference rules.
- Test inherited reads, private partial-block COW, explicit zero masks,
  immutable parents, sibling preservation, and deeper chains.
- Preserve parent references when staging and publishing a child in another
  directory. Validate the published location rather than only a temporary path.
- Complete caller-owned graph snapshot creation, opening, selection/revert,
  leaf deletion, flattening, content-preserving rebase, and new-output merge.
  Define revert as selecting a disk state unless a separate mutation contract
  explicitly permits replacing an existing image.
- Require explicit dependency authority for destructive operations; reject
  unknown ownership or unsafe branches. Design multi-file VMDK publication and
  deletion before enabling those management operations.

### Zero, trim, and compaction

- Define trim as zero-readable discarded ranges with an explicit choice between
  required deallocation and allowed zero fallback. Never silently reveal a
  parent's data or promise physical reclamation from logical zeroing.
- Test host hole punching where supported, unchanged virtual capacity,
  unaligned neighboring bytes, invalid ranges, and unsupported host filesystems.
- Implement each container's native mapping/refcount/bitmap discard separately;
  test allocation ownership, full and partial allocation units, and recovery.
- Keep initial compaction as verified new-output rewriting. Preserve inherited
  nonzero bytes and do not infer guest free space from container holes.
- Introduce in-place compaction only with an explicit relocation and recovery
  protocol plus measured reclamation evidence.

### Advanced format profiles

- QCOW2: complete shared-table COW and refcount growth, then internal snapshot
  ownership reconstruction and list/create/open/revert/delete. Treat extended
  L2, external data, compressed export, and persistent bitmaps as separate work.
- VDI: complete native differencing interoperability, native capacity changes,
  zero-map/discard updates, and identifier lifecycle validation.
- VMDK: complete writable flat/split extents and multi-file recovery/publication;
  add stream-optimized import/export and further profiles only by explicit name.
- VHDX: complete parent linkage, partial-block sector inheritance, native log
  recovery, and sector-bitmap mutation before optimizing partial-block COW.
  Validate native Windows behavior for both supported logical sector sizes.
- Repair: expose read-only diagnostics first. Each mutating repair needs a
  separately specified precondition, recovery strategy, and regression fixture.
- Incremental backup: start with bounded logical export; add persistent change
  tracking only after format-specific bitmap ownership and lifecycle support.

### CLI, fuzzing, and release completion

- Expose implemented library operations through the CLI with explicit format,
  parent authorization, shrink/discard policy, and stable machine-readable
  results. Do not advertise commands that only exist in this plan.
- Extend honggfuzz reader targets with stateful models for every writer and
  graph operation; include deterministic failure injection and recovery replay.
  Follow `../vlmcsd-rs/fuzz`, use `.#fuzz`, and retain `.#nightly` independently.
- Freeze production and harness sources during each measured campaign. Run
  deterministic replay, bounded smoke, and sustained campaigns as distinct
  evidence; retain reproducible receipts and minimized regressions.
- Run the required locked Rust checks and independent tool tests for each
  checkpoint. Complete native VirtualBox, VMware, and Windows/Hyper-V gates
  before declaring their respective interoperability profiles validated.
- Benchmark random/sequential I/O, metadata memory, conversion throughput,
  COW amplification, trim effectiveness, and recovery overhead with reproducible
  datasets. Set release limits from measurements rather than guesses.
- Publish the tested support matrix, persistence guarantees, platform limits,
  recovery procedures, and outstanding gates. A format family remains partially
  supported while any advertised operation lacks its acceptance evidence.

## Primary references

- [QCOW2 format specification](https://www.qemu.org/docs/master/interop/qcow2.html)
- [QEMU disk format profiles](https://www.qemu.org/docs/master/system/images.html)
- [qemu-img operations](https://www.qemu.org/docs/master/tools/qemu-img.html)
- [Microsoft VHDX log replay requirements](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/0d588e33-23a6-4c71-b27f-87d97ac3e914)
- [VirtualBox virtual storage documentation](https://docs.oracle.com/en/virtualization/virtualbox/7.1/user/storage.html)
- Local honggfuzz workflow reference: `../vlmcsd-rs/fuzz/README.md`.

Consult the full versioned format specifications and upstream implementations
when implementing each profile; this plan is not a replacement for them.
