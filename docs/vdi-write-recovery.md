# VDI discard and recovery

`VdiWriter::discard(offset, length, policy)` supports native dynamic and explicitly authorized differencing images on Linux, with allocation units no larger than 1 MiB. The offset must start an allocation unit; length must cover complete units or end exactly at the virtual capacity, allowing a clipped final logical block. Empty ranges leave the image unchanged and return Zeroed.

Strict requests validate range, profile, single-link ownership, and exact physical tail before changing the modification UUID. Fixed images, unaligned ranges, larger units and foreign trailing bytes require an explicit zero fallback. Fallback writes report Zeroed and promise no allocation reclamation. Invalid ranges never fall back.

## Dense physical ownership

VDI allocations form a dense physical array. Discarding a private interior unit copies the last allocated unit into the discarded slot and changes that last unit's logical owner mapping to the slot. The target receives the native ZERO marker, allocated count decreases, and the final physical unit is truncated. Tail discard skips the copy. Discarding a free/inherited unit journals ZERO without allocating payload: parent contents cannot reappear. An already explicit-zero unit is a native no-op. Deallocated means native container ownership was released or the native zero mapping accepted; physical truncation occurs only when a private unit existed.

Each unit is a separate transaction. Multiunit requests are serialized but not atomic; an I/O error may leave a completed prefix and an interrupted next unit requiring recovery. Capacity is unchanged. Each record carries at most one allocation unit of truncated tail, normal patches no larger than 1 MiB, and at most 4 MiB of serialized journal data. Full-file identity hashing is repeated per transaction; operations are bounded by the writer's 32 GiB capacity ceiling and allocation-unit count. Reclaimed units can be allocated again through the existing append allocator.

## Shrink journal contract

The existing hosted-image journal now permits a single terminal tombstone: its offset equals final physical length, its old bytes cover exactly the removed suffix (at most 1 MiB), and its new bytes are empty. Normal patches lie entirely before the new end and cannot overlap the archived tail. Malformed or incomplete archives fail before mutation.

Before any replay, the journal reconstructs the exact original view, including archived bytes already truncated from the backing file, and verifies its SHA-256 preimage identity. Surviving bytes must match old/new transaction states; foreign surviving tails and changes outside patches fail closed. Both original and proposed VDI ownership and parent chains are validated through immutable overlays.

The journal and parent directory are persisted before publication. Payload relocation precedes mappings and allocated-count updates. Updated bytes are synced before shrink; truncate is then synced before sidecar removal and directory sync. Interrupted shrinking files can have lengths between the original and final length; archived tail reconstruction makes redo idempotent. Original/proposed validation runs again on recovery. A failed commit poisons the writer; readonly opens reject pending evidence and writable child recovery requires the same authorized parent chain.

## Evidence

Tests cover interior/tail relocation, inherited zero masks, clipped final units, multiunit requests, repeated discard, allocation reuse, invalid-range and unsupported-profile UUID preservation, hard-link fencing and foreign tails. Fault injection covers all eleven transaction boundaries for standalone and child interior discard, including cuts after truncation; recovery preserves unrelated blocks and parent bytes and is idempotent. Shared-journal tests independently check complete archive coverage, partial physical shrink, malformed tombstones and foreign preimage data.

The ignored native oracle checks and flattens a reclaimed interior VDI using VirtualBox and QEMU. These are host format tests, not Windows servicing or capture correctness tests. Native allocation metadata follows the [Oracle VDI backend](https://github.com/VirtualBox/virtualbox/blob/main/src/VBox/Storage/VDI.cpp); persistence uses this library's explicit hosted-image sidecar rather than a native VDI log.
