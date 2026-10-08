# Common operation budgets, progress and cancellation

This is a development API after the published 0.2.0 checkpoint. It is not
included in that registry artifact.

`copy_image_with_context`, `hash_image_with_context` and
`compare_images_with_context` share `OperationContext` with
`check_payload_with_context`, `check_image_with_context`,
`convert_image_with_context`, `compact_image_with_context` and
`resize_image_with_context`. Existing copy/hash/compare
APIs delegate to these implementations using default limits; the old copy
cancellation callback retains its polling boundaries.

```rust,no_run
use std::ops::ControlFlow;
use virtdisk::{OperationContext, OperationLimits, OperationProgress};

let limits = OperationLimits::default()
    .logical_bytes(1024 * 1024)
    .io_operations(64)
    .scratch_bytes(32768)?;
let mut observer = |progress: OperationProgress| {
    eprintln!("{}/{}", progress.completed_bytes, progress.total_bytes);
    ControlFlow::Continue(())
};
let mut context = OperationContext::new(limits).with_observer(&mut observer);
# let _ = &mut context;
# Ok::<(), std::io::Error>(())
```

Limits use private fields and validated builders. Byte and top-level I/O limits
default to `u64::MAX`; known logical capacity bounds each operation. Callers can
tighten these limits, including to zero for empty work. Scratch can be tightened
from the existing 128 KiB combined comparison ceiling. Each buffer remains at
most 64 KiB, preserving legacy read granularity; comparison uses two buffers.
Scratch allocations are fallible and independent of logical image size.

Copy/hash/compare check their complete logical request before callbacks,
allocation or I/O. Materialization checks each export pass and verification
separately because sparse allocation is discovered by scanning. A typed
`OperationLimitExceeded` reports the resource, ceiling and requested total via
`io::Error::get_ref()`. Requested counts use `u128` to detect a request beyond
`u64` rather than wrapping it. Comparison budgets the complete equal-capacity
request even if a difference later permits early exit. Unequal capacities
return false without I/O or notification.

Usage accumulates across operations on a reused context:

- Logical bytes count successfully processed chunks. Copying counts chunks
  whose writes succeeded; hashing counts hashed chunks; comparison counts pairs
  examined, including a chunk containing the first difference; payload validation
  counts completely read chunks.
- I/O calls count attempted top-level `ReadAt`/`WriteAt` calls, including failures.
- Peak scratch records the largest combined requested buffer size, excluding
  allocator overhead and backend caches.

These counters do not measure physical I/O, backend allocations, decompression,
metadata work or elapsed time. Failed writes may have partial effects beyond
the completed-byte counter. Parser limits and native writer bounds still apply
independently. The context itself does not establish durability. Conversion and compaction
retain their explicit sync, verification and publication contracts.

Observers are synchronous borrowed `FnMut` callbacks returning `ControlFlow`.
They run before work and after complete chunks, outside backend calls and native
transactions. `Break(())` returns `Interrupted` with `OperationCancelled`.
Cancellation after a final chunk still returns an error, although all logical
bytes may have been processed. Completed writes remain. Empty primitive operations notify
once and require no scratch or I/O; a mismatched copy fails before notification.

Progress includes a typed `OperationPhase`: generic operations use `Processing`,
container checking reports `MetadataValidation`, and logical sweeps report
`PayloadValidation`. Metadata validation has no logical byte measure, so its
completed/total byte fields are zero; this does not indicate completed validation.
Successful checks return a `CheckReport` only after the requested work finishes.

Whole-image checking notifies before opening, during QCOW2 ownership/compressed
descriptor validation and after constructors. Other families' bounded constructors
remain non-interruptible internally. The typed cancellation error is preserved
through QCOW2's cancellation predicate. No check modifies or repairs image bytes.
Opening and metadata/compressed-descriptor audits use their existing independent
parser budgets; operation byte/I/O/scratch limits cover only the logical sweep.
The payload request is preflighted after opening/metadata validation and before
payload reads. Metadata work is not silently counted as payload access.

## Conversion and compaction

Both context-aware materialization APIs cover raw, QCOW2, VHDX, VDI and VMDK.
They create a private sibling staging image, sync it, compare its logical bytes
with the immutable source, validate QCOW2 ownership and then publish using a
no-overwrite hard link. Errors or cancellation before publication leave the
requested output absent; staging cleanup is best effort. No cancellation is
reported after publication, and a directory-sync failure may leave a published
output, as in the existing publication contract.

`AllocationScan` measures a complete source pass used to discover zero native
units. `ImageExport` measures source bytes read for materialization; sparse
VDI/VHDX export only reads allocated units in that phase, while sparse QCOW2
reads the entire source again. `OutputVerification` measures the logical range
once while comparing both readers. Each phase reports its own exact total.
Repeated source passes count again toward cumulative logical bytes. Failed
source reads charge the attempted call but no completed bytes. Native source
callbacks occur after complete reads, before the corresponding payload write.
Raw export counts completion after any required write succeeds.

Native exporter file writes, metadata allocations and syncs remain backend
work outside operation I/O accounting. Raw export counts both reader and writer
calls, omitting writes for zero compaction chunks. It preflights the worst-case
write count; usage records the actual attempted calls. Raw export adapts chunks
to scratch, and comparison uses two adaptive buffers. Native exporter streaming
requires fixed combined buffer ceilings: QCOW2 131072, VMDK 67584, and VDI/VHDX
65536 bytes. Smaller limits fail before callbacks or source I/O. Metadata/maps,
allocator overhead and backend caches are independent bounded allocations.
This API does not claim that operation scratch measures process memory.

Quota checks occur per pass; a later refusal can follow earlier staged work.
Usage remains on the context, while the staged image is discarded. The
`Publication` event has zero byte totals and is the final cancellation boundary,
after logical and ownership validation but before the hard link. Metadata and
individual backend calls cannot be interrupted internally. Cancellation does
not provide source snapshot isolation; every source must remain immutable.
Observers remain borrowed synchronous `FnMut` and need not be `Send` or `Sync`.

## Resizing into a new image

`resize_image_with_context` materializes a resized independent image in every
supported output format. It retains the explicit `ShrinkPolicy`: `Reject`
refuses reductions before callbacks or source I/O; `RequireZero` scans the
removed tail; `AllowDataLoss` permits removing nonzero bytes without scanning.
Growth has no tail validation. This changes disk capacity, not guest partition
or filesystem structures. The original source and dependencies remain immutable.

`TailValidation` preflights the complete removed range, uses one adaptive
fallible buffer, and reports completed bytes relative to that tail. Each
successfully examined chunk is counted, including a chunk containing nonzero
bytes. Observers run before scanning and after each completed chunk. Nonzero
tail content returns `InvalidInput`; a source failure preserves its I/O kind,
counts the attempted call and does not count an incomplete chunk. Cancellation
after a successful chunk can take precedence over nonzero-tail refusal.

Tail scanning, export and verification share one cumulative context. A later
materialization quota/profile refusal retains earlier tail-scan usage; staging
cleanup and no-overwrite publication retain the conversion contract. Native
streaming scratch floors apply to materialization and can be refused after
successful tail validation. Equal-capacity rewrites and growth skip the tail
phase. Synthetic zero growth counts as processed logical bytes and top-level
reads of the resized facade, even where no physical source read occurs.

The default `resize_image` delegates using an unrestricted default context.
The CLI `resize` accepts operation quotas and progress, including
`tail-validation` and the final `publication` boundary. It keeps the existing
policy arguments and does not authorize in-place native resize or recovery.

Native resize/recovery and snapshot/chain contexts remain open. Their
transaction boundaries must be honored before enabling cancellation.

## CLI operation quotas

Leading `--operation-limit NAME=INTEGER` applies common operation quotas to
`hash`, `compare`, `check`, `convert`, `compact`, and copy-based `resize`. Names are `bytes` (logical bytes), `io` (top-level
I/O calls), and `scratch` (combined requested scratch bytes). Byte/I/O limits
may be zero; scratch must be 1..=131072. Repetition retains the smallest value.
Invalid controls and controls for unsupported commands are rejected before
opening files. These controls can be interleaved with leading parser controls.

```sh
virtdisk --parser-limit work=100000 --operation-limit bytes=1048576 --operation-limit scratch=4096 hash disk.raw raw
virtdisk --operation-limit io=1000 compare left.raw raw right.raw raw
```

Hashing and comparison preflight their known payload request after opening.
Comparison counts the logical range once and two attempted reads per chunk;
scratch is shared by its two buffers. Checking accounts for the requested
current logical payload sweep after metadata validation. Structure-only checks
therefore work with zero byte/I/O operation quotas; parser budgets still apply.
Metadata parsing, decoding allocations and output formatting are outside these
operation quotas. Quota failure exits with status 2 and produces no success
output; a completed unequal comparison retains status 1. These are unpublished
changes after 0.2.0.

## CLI progress

Leading `--progress` enables synchronous JSON-line progress on stderr for
`hash`, `compare`, `check`, `convert`, `compact`, copy-based `resize`, and `zero`. It can be interleaved with leading parser and
operation limits. Result output stays on stdout. Each record contains `type`
(`progress`), `phase`, `completed_bytes`, `total_bytes`, `logical_bytes`,
`io_operations`, and `peak_scratch_bytes`. Phase names are `processing`,
`metadata-validation`, `payload-validation`, `allocation-scan`, `image-export`,
`output-verification`, `tail-validation`, `zeroing`, and `publication`; a future unrecognized phase
is represented as `unknown`.

```sh
virtdisk --progress --operation-limit scratch=4096 hash disk.raw raw
virtdisk --progress check disk.qcow2 qcow2 payload
```

Metadata byte counters are zero because metadata work has no logical byte
measure. Payload events include the initial boundary and each completed chunk,
including the final partial chunk. Empty work reports zero totals. Progress
records describe completed work; the result and exit status establish success.
Known payload quota refusal occurs before payload progress callbacks. Checking
can already have emitted metadata progress before a payload quota refusal.

Observers write synchronously; a slow progress consumer can delay the operation.
Failure to write progress stops the read operation at that boundary, preserves
the I/O error and exits with status 2 without a success result. Diagnostic
failure on a closed stderr is handled without panicking. Other commands reject
`--progress` before file access. CLI signal cancellation and native management-context
integration remain open. These changes are unpublished after 0.2.0.

## Existing-image zeroing

`zero_image_with_context` checks the whole logical range and known byte/I/O
quotas before its initial `zeroing` callback. It dispatches native `write_zeroes`
calls of at most 64 KiB, counts attempted calls and completed bytes, and invokes
observers after each call returns. Empty ranges produce one callback and no I/O.
It owns no scratch buffer; backend allocations are outside scratch accounting.

Cancellation and failure retain completed prefixes. Native profile validation
occurs per call, and a failed call may have partial effects beyond the completed
counter. An `OperationError` identifies the failed chunk. The function does not
flush; callers must exclude concurrent management operations.

CLI `zero` accepts leading operation limits and `--progress`, including before
`--recover zero`. Supplying either selects bounded execution. Without these
controls the CLI preserves whole-call native validation. Successful CLI zeroing
explicitly flushes. Opening, recovery and flush are outside the operation budget;
explicit recovery may mutate an image before a later quota refusal.

## External snapshot graphs

`ImageGraph` exposes `snapshot_with_context`, `snapshot_as_with_context`,
`flatten_with_context`, `rebase_to_with_context`, and `merge_to_with_context`.
Existing methods delegate using default contexts. Identity, authorization and
ancestry checks retain their existing contracts; callers must supply all known
dependents and exclude concurrent file and graph mutations. These operations
create new outputs and keep parents, selected states and sibling branches
immutable. Revert remains selection of a state through `reader`.

Snapshot creation checks native metadata and compares the entire child with its
parent in `output-verification`, with bounded scratch and counted read calls.
A final `publication` callback permits cancellation before the output hard link
is created. Native creation, metadata allocation and validation, opening and
synchronization are backend work outside operation accounting. Comparison quotas
are checked before comparison reads, after native staging may have been created.
A zero-byte `metadata-validation` callback precedes snapshot staging.

Rebase remains a new QCOW2 child over an authorized raw/QCOW2 parent. Its
`image-export` pass uses two fallibly allocated buffers, each at most 64 KiB,
adapted to the combined scratch limit. It preflights one logical pass and a
worst-case three I/O calls per chunk: selected-state read, parent read and native
write. Only attempted calls are charged. Unchanged chunks skip writing; ranges
beyond a shorter parent inherit zero and skip parent reads. Bytes count once per
completed chunk. Native allocations and flush are outside accounting. Buffers
are released before the full `output-verification` comparison and final
`publication` callback. Callback cancellation never interrupts a backend
transaction. The borrowed observer need not implement `Send` or `Sync`.

Flattening and ancestry-checked new-output merge use the existing materialization
phases and budgets. One context carries cumulative usage across passes and graph
operations. A later quota refusal retains earlier usage and removes unpublished
staging on a best-effort basis, without registering a child edge. Cancellation
at the final publication callback has the same cleanup effect. Directory sync
or registration failures after publication can leave the output present without
an edge; callbacks cannot revoke published files. Graph manifest persistence is described in [graph-manifests.md](graph-manifests.md);
branch-aware in-place commit/rebase and graph CLI commands remain separate work.

Graph parser budgets are a separate control: `ImageGraph::open_with_limits`
and `GraphManifest::open_graph_with_limits` retain shared parser accounting
across graph reads and staged snapshot/rebase validation. They do not replace
operation payload quotas or native writer limits. See
[graph parser limits](graph-manifests.md#cumulative-graph-parser-limits).

Snapshot generation publication adds a final `GenerationPublication` phase
following preparation of both the image and manifest. The earlier `Publication`
phase publishes the image inside private staging; cancellation at either
boundary prevents the final generation from appearing. No observer runs after
the final directory rename. See [atomic snapshot generations](graph-manifests.md#atomic-snapshot-generations-on-linux)
for post-publication error handling and platform scope.

Owned graph leaf deletion uses `MetadataValidation` followed by the final
`SnapshotDeletion` cancellation boundary. It consumes no payload quota and emits
no callback after unlink. Source identity checks follow the final callback;
post-unlink directory-sync errors retain the updated graph and removed file.
See [controlled leaf deletion](graph-manifests.md#controlled-leaf-deletion).

## Native capacity changes

`ImageWriter::resize_with_context` retains exclusive mutable access and existing
backend locks. A `RequireZero` shrink preflights and scans the resolved removed
tail in `TailValidation`, using fallibly allocated scratch of at most 64 KiB,
adapted to the caller ceiling. Each completed chunk counts once, including a
chunk with nonzero bytes; attempted facade reads count even if they fail. Quotas
preflight the entire scan plus one native resize call before callbacks or I/O.
Growth and explicit data-loss shrink consume zero logical bytes and one attempted
native call. Usage is cumulative across operations; no implicit flush occurs.

A final `NativeResize` callback allows cancellation before starting the backend
capacity transaction. No callback runs inside or after it. Tail refusal, quota
refusal and cancellation leave the image unchanged by this operation; native
backend errors retain their existing partial-mutation/recovery contracts.
Successful zero-tail validation is passed to the backend as an approved tail
removal, avoiding a second scan under the same retained access. Parents must
remain immutable, dependents and external readers excluded, and guest filesystem
resize remains the caller's responsibility. Backend opening, metadata validation,
allocation, journal reads/writes and synchronization remain outside context
accounting. Unsupported alignment, capacities or profiles can fail at native
dispatch after scan usage has accrued.

The CLI accepts `--operation-limit` and `--progress` for `resize-native`, with
optional explicit parent paths after the shrink policy. Without controls it
retains the existing whole-call dispatch. Recovery still requires `--recover`;
operation controls do not authorize journal replay.

## Native disk snapshot lifecycle

`ImageWriter::create_snapshot_with_context`, `delete_snapshot_with_context` and
`revert_snapshot_with_context` expose the existing bounded native QCOW2 lifecycle
through the common operation context. They preflight one facade I/O call, then
emit a single zero-byte `NativeSnapshotCreation`, `NativeSnapshotDeletion` or
`NativeSnapshotRevert` callback before starting the native operation. Cancellation
or quota refusal starts no mutation and charges no call. Native dispatch counts
one attempted call, including unsupported profiles, malformed IDs or backend
failures; it consumes zero logical bytes and no facade scratch.

There are no callbacks during or after the transaction. Opening, metadata
validation, allocations, journal I/O and synchronization remain backend work
outside context accounting. Flush remains explicit. Backend errors preserve their
existing recovery effects; contexts do not imply rollback or authorize recovery.
Supported profiles, immutable parent authorization, saved-state preservation and
external dependent/access exclusion are unchanged. The CLI accepts operation
limits and progress for `snapshot create`, `delete` and `revert`, preserving
whole-call dispatch when no controls are supplied. `--recover` remains explicit.

## Native discard and preallocation

`ImageWriter::discard_with_context` and `preallocate_with_context` check the whole
range and preflight its logical-byte quota plus one attempted native call before
callbacks or I/O. `NativeDiscard`/`NativePreallocation` provide one final
cancellation boundary. Successful calls charge the requested logical range;
failed native calls charge one attempted call but no completed bytes, because
partial backend work is not measured. Empty calls still count one native attempt.
No facade scratch is allocated, and flush remains explicit.

Native alignment, deallocation versus zero fallback, parent masking and host
preallocation semantics are retained. No callback runs during or after native
dispatch, so cancellation cannot interrupt a backend transaction or revoke its
effects. Backend reads/writes, journal work, allocation, zero-fallback buffers,
metadata and synchronization remain outside these counters. One native call may
perform multiple backend transactions; failure retains their existing partial
completion/recovery rules. These quotas bound the requested facade range and
native call count, not physical disk I/O. Capacity/dependency management and
external mutation must be excluded as required by the concrete profile.

CLI `trim` and `preallocate` accept operation limits/progress; without controls
they retain whole-call behavior. Parent grants and explicit `--recover` remain
separate authorization controls. Preallocation currently supports raw files;
container requests remain `Unsupported` without substituting zero writes.
