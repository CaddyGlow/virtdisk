# Native QCOW2 snapshot deletion and revert

Status: implemented for the bounded lifecycle profile documented below. Creation and read-only views have separate
contracts in `qcow2-snapshot-write.md` and `qcow2-snapshots.md`. This design
continues the complete implementation plan; the bounds below are initial
transaction profiles, not a replacement for broader lifecycle support.

`delete_snapshot` and `revert_snapshot` operate on an exclusively mutable
writer, then dispatch through the common writer and CLI. Begin with the same
Linux standalone v3/64 KiB/16-bit profile, private active L1 clusters, zero VM
state and bounded directory/L1/journal resources. Validate the original complete
ownership graph before planning. Missing IDs and unsupported requests must leave
the whole container unchanged. No guest memory state or hypervisor registration
is changed.

Deletion removes one saved disk state, never its surviving siblings. Preserve
the retained raw directory records, including bounded unknown extras. Publish
a replacement directory, or clear count/offset when the final state is deleted.
Calculate signed reference deltas for the removed L1 allocations, every removed
L1-to-L2 reference, and every reachable payload reference. Repeated pointers
contribute repeated owners; deduplicating them would undercount releases.
Include directory replacement/release in the same plan. Recompute active copied
flags using final counts; inactive flags may remain stale. Freed container
clusters need not shrink the host file.

Revert selects a saved disk state while retaining that state and every sibling.
Plan the difference between old active and selected L2/payload ownership.
Append a fresh private active L1, release the old active L1 owners, and publish
its pointer, entry count and saved virtual capacity together. The selected
snapshot keeps its own saved L1. Recompute copied flags from final ownership;
do not trust the inactive saved flags. Prepare the replacement mapping cache
and capacity before commit and install them only after successful publication.
Payload writes after revert must use COW and preserve every saved state.

Use the existing redo mechanism to validate both proposed and original states,
publish recoverably, and poison the live writer whenever replay is required.
Recovery can finish an operation whose caller received an interruption; callers
must inspect the requested ID/state after reopening. Do not build deletion or
revert out of independent unjournaled header/refcount edits. Handle compressed
snapshot owners explicitly using the sector extent rules in the [QEMU format specification](https://www.qemu.org/docs/master/interop/qcow2.html#cluster-mapping). Unsupported resource or format profiles fail before publication.

Required failing behavioral tests precede implementation:

- First/middle/final deletion; deletion of unchanged/shared and private payload
  states; exact surviving bytes, ownership, directory metadata and active flags.
- Missing IDs and unsupported metadata; exact no-mutation checks and a usable
  retained writer after preflight rejection.
- Repeated references, preallocated zero mappings and arbitrary ID/name bytes.
- Revert across larger, smaller and empty saved capacities; stale inactive flags;
  partial writes after revert with selected/sibling byte models unchanged.
- Every actual changed metadata patch and persistence interruption boundary;
  normal reopen, repeated replay and exact current/saved model verification.
- Independent QEMU check/list/export for all surviving snapshots and the active
  state; interoperability with QEMU-created states and subsequent QEMU mutations.
- Stateful Honggfuzz create/write/delete/revert/reopen sequences with independent
  per-state models, authorized paths and explicit operation/resource bounds.

The native ownership/flag rules come from the
[QCOW2 specification](https://www.qemu.org/docs/master/interop/qcow2.html#snapshots).
The publication algorithm above uses this repository's transaction engine. Interoperability evidence and
limitations are recorded below.

## Implemented bounded lifecycle profile

`Qcow2Writer::delete_snapshot` and `revert_snapshot` now implement the algorithms
above using one native metadata redo transaction. Both return the selected
snapshot metadata. Missing IDs return `NotFound` without modifying the image.
Deletion releases container owners without shrinking or punching holes in the
host file. Final deletion clears both the native snapshot count and pointer.
Revert retains every saved state, installs a freshly allocated private active L1,
and changes active capacity and the mapping cache only after a successful commit.

Initial mutation support requires Linux, standalone v3, 64 KiB clusters, 16-bit
refcounts, private active L1 ownership, zero VM state, at most 64 saved states,
one cluster each for the active/selected L1 and directory, and a physical image
no larger than 33 GiB. Revert applies the 32 GiB sector-aligned virtual-capacity
ceiling. Planning is capped at 16 million work items and the existing 16-cluster
journal budget. Deletion releases each physical cluster covered by every compressed
sector extent, including packed and unaligned owners. Revert preserves saved
compressed storage and materializes its active compressed grains into private
uncompressed payloads and cloned L2 tables within the same recovery transaction.
Ordinary descriptors in mixed tables retain exact per-state ownership. Decoding
uses the bounded snapshot reader; the 16-cluster transaction limit additionally
bounds materialized output. Plans beyond these limits fail before mutation.
Active compressed writable profiles, larger
metadata transactions, backing-chain mutations, renaming and VM capture remain
unfinished work in the complete plan.

Regression coverage includes repeated L2/payload references, preallocated zero,
opaque snapshot extras, arbitrary ID/name bytes, capacity changes to smaller,
larger and empty states, retained-state COW, and every actual changed patch and
persistence cut for deletion/revert fixtures. Native QEMU checks and subsequent
native apply/delete operations validate interoperability. QEMU's read-only
`snapshot_load_tmp` used by `convert -l` switches L1 without changing virtual
capacity; the oracle therefore verifies saved bytes at the current export size
and separately verifies changed capacity through native active-state conversion.
