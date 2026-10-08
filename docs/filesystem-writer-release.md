# Filesystem writer release contract

Status: owner release published and verified on 2026-10-07. This implements the
owner release and registry dependency evidence portion of
[the release plan](../FILESYSTEM_WRITER_RELEASE_PLAN.md).

Published release: **[0.2.0](https://crates.io/crates/virtdisk/0.2.0)**.
Its registry artifact SHA-256 is
`ed266f4c5cae3fba8b0470b42cfe43dd379c12ecfaab31a52f56456ce0fe4f8d`.
The owner manifest and local path-dependency locks declare this version. The
published 0.1.0 remains unchanged. Exact package, API, source and verification
evidence is recorded in
[the release receipt](evidence/filesystem-writer-release-0.2.0.json).

## Public API required by the consumer

The library must export `ImageWriter::{open,open_chain,create,read_exact_at}`,
`WriteAt::{len,write_all_at,flush}`, `InspectImage::inspection`, `ImageFormat`,
`ImageOperation`, `Capability`, and inspection geometry. These library APIs do
not require the optional `cli` feature. The current minimum Rust version is 1.99
and the crate uses edition 2024.

`ImageWriter` provides synchronized logical reads through its inherent
`read_exact_at` method and implements `WriteAt` and `InspectImage`. It deliberately
does **not** implement `ReadAt`: that trait promises an immutable logical source
whose length remains fixed for its reader lifetime. Reopened immutable `Image`
readers supply the compatible `ReadAt` API after mutation has ended.

An operation is authorized by the actual handle, not the filename or format
family. Consumers query `inspection().capabilities.get(operation)` and require
`Capability::Supported` for read, write and flush. Support remains subject to the
opened profile, bounds, resource limits and I/O failures. Inspection reports
retained validation facts, not filesystem validation or a new authenticity check.
Logical and physical sector sizes are optional facts; absent values must remain
unknown or be supplied explicitly by the caller. Allocation block size is neither
a sector size nor an atomic write guarantee.

## Supported operating conditions

All mutation is offline. Writers retain exclusive OS file locks and serialize
their operations, but cooperative locks do not prevent arbitrary external access.
The caller must exclude mounted use, external readers or writers, management
operations, and dependent-image mutation. A session must own its writer privately
and retain it through its final flush and close. Native resize must not race a
bounded filesystem session, and a zero tail does not establish safe filesystem
or partition shrinkage.

| Family | Release scope to preserve |
| --- | --- |
| Raw | Regular-file positional writes, zeroing and flush; host support controls sparse creation. Native hole punching and preallocation are Linux operations. |
| QCOW2 | Bounded writable v3, 64 KiB clusters and 16-bit refcounts. Linux allocation, COW, authorized parents and journal recovery; native resize and snapshots have narrower standalone profiles. |
| VDI | Bounded v1.1 fixed/dynamic and explicitly ordered UUID-linked parents. Linux journaled allocation and inherited-block COW; native discard/resize require eligible dynamic profiles. |
| VMDK | Bounded hosted sparse and explicitly authorized descriptor extents. Sparse allocation and backed split COW use Linux journal recovery; missing tables require the documented metadata-space and resource bounds. Native resize/discard are narrower hosted profiles. |
| VHDX | Clean bounded fixed/dynamic and authorized differencing profiles, logged allocation and inherited-sector writes. Pending logs require explicit authorized recovery. Native dynamic discard and resize are Linux profiles. |

This table is not a promise that every file in a family opens writable on every
platform. The concrete writer documentation and capability report remain the
selection contract. See the [README profile table](../README.md),
[split VMDK transactions](vmdk-split-transactions.md) and
[split parent contract](vmdk-split-parent-cow.md).

Backing files must remain immutable throughout opening, reads, writes and
recovery. Explicit path authorization does not grant permission to mutate a
parent. Backed split VMDK additionally retains shared read-only parent locks and
binds replay to all ancestor identities, lengths, content digests and topology.
Ordinary read-only `RawDisk` opening does not itself lock external writers or
create a snapshot.

## Errors and durability

Checked range rejection does not extend capacity. A successful positional write
means the requested logical bytes were accepted; it does not make an arbitrary
multisector request atomic or durable. An I/O failure can leave a prefix or part
of the request changed. Journaled metadata recovery protects its documented
transaction unit, not a generic filesystem operation. Some failed container
operations poison further mutation and require authorized recovery/reopening.

The consumer must mark a write or flush error as sticky uncertainty. A later
successful flush does not prove rollback or clear that uncertainty. After an
uncertain operation, filesystem-specific validation and recovery are the
consumer's responsibility. Neither this API nor its range checks authorize
filesystem mutation or establish filesystem recovery correctness.

`flush` requests host persistence of completed image writes and required container
metadata. It is distinct from publishing a new pathname. `ImageWriter::create`
does not overwrite an existing output, can leave a partial output on failure,
and does not sync the containing directory. Callers performing creation, rename
or publication must satisfy their directory durability protocol separately.
Operation-specific conversion/publication helpers have their own contracts.
Process-kill replay tests establish only their tested interruption behavior;
they are not proof of power-loss, device-cache or storage-stack durability.

## Consumer audit

The read-only audit inspected `../partmgr/integration/storage` without editing or
running it. Its manifest requests `virtdisk = "0.1.0"`; the release plan records
prior compiled testing against a command-only local patch, not registry evidence.

`WriterSession` consumes `ImageWriter` into a private storage adapter and retains
it inside `BoundedSession`. It checks read/write/flush capabilities, reconciles
optional sector facts with supplied geometry, and exposes bounded reads, writes,
zeroing, explicit flush and sticky uncertainty. Its source tests cover real raw
and Linux QCOW2 read-after-write, range rejection, retained locks and complete
logical-image reopen comparison. Fault wrappers around a real raw writer cover
partial write and flush errors, including uncertainty surviving a later flush.
These tests establish the basic bridge when run against the selected dependency;
wider native container and performance acceptance remain separate gates.

## Remaining release gates and receipt

Before publication, the owner must complete the required format, all-target
all-feature Clippy and all-feature locked tests on the release sources, retain
writer recovery and independent native gate evidence, and verify the package
contents. Fuzzing is currently paused and no new fuzz receipt is implied.

These owner gates passed: formatting, Linux and Windows MSVC all-target/all-feature
Clippy, and the required serial suite with 406 passed, zero failed and 68 optional
tests ignored. Independent backed split QEMU gates passed separately, including
144 recovered full-array conversions and an actual native version-2 producer.
Publication dry-run verified the packaged library and optional CLI; upload then
completed successfully. All 158 downloaded package files matched the tested
owner sources where comparable. The published VCS metadata records the source
revision and a dirty worktree; no release tag or new commit is implied.

The inspected common VMDK dispatch gap is corrected: `ImageWriter::open_chain`
selects the parent-aware split writer for sparse descriptors and preserves flat
descriptor handling. A meaningful failing common-API regression now passes with
cross-extent COW, parent lock retention, parent preservation and reopened bytes.
Logs: `/data/cache/virtdisk-common-split-red.log` and
`/data/cache/virtdisk-common-split-green.log`. The first release suite was stopped
with exit 130 after this finding; its partial log is not passing release evidence.

After publishing, record the exact version, registry source/checksum, packaged
source hashes and VCS state, public features, Rust/platform versions and terminal
verification results.
The consumer owner then removes its command-only patch, updates registry version
and lock resolution, verifies the resolved source is crates.io, and reruns raw,
QCOW2, uncertainty, applicable native-container and performance gates. A local
package check or patched consumer pass cannot close this dependency gate.

The isolated registry consumer passed five tests for the required default-library
API, raw/QCOW2 sessions, lock retention, bounds, full-image reopening and sticky
partial-write/flush uncertainty. Its native QCOW2 bridge gate passed `qemu-img
check`, full raw conversion and unchanged-source comparison. Cargo.lock resolves
0.2.0 from crates.io with the published artifact checksum and no virtdisk path
patch; resolved features are empty. Partmgr itself remains a frozen local source
snapshot, explicitly separate from virtdisk's registry evidence. The original
sibling checkout was not edited and its owner still needs to update its manifest
and lock resolution.

Small debug-profile measurements cover eight sequential writes, 64 random writes
and reads, flush timing and exact whole-image reopening for raw and QCOW2. They
are retained measurements, not sustained performance qualification or an invented
acceptance threshold. Wider native/container/performance profiles remain separate
acceptance work. Fuzzing remains paused.

A minimal independent smoke artifact should be a fresh standalone Cargo project
with `virtdisk = "=0.2.0"` and no path patch. Compile the required imports and
calls above, create a temporary raw image, inspect capabilities, write/read a
bounded pattern, flush, drop the writer and reopen an `Image` through `ReadAt` to
compare the full logical model. On Linux repeat with the supported QCOW2 profile.
Compile authorized `open_chain` dispatch and exercise applicable native fixtures
separately. Keep its Cargo.lock and commands with the receipt. Execution and
registry verification are owner gates, not results claimed by this document.
