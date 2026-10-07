# Native QCOW2 disk snapshot creation

`Qcow2Writer::create_snapshot` creates an internal disk-only snapshot while the
existing writer retains its exclusive lock. Initial support is Linux, standalone
QCOW2 v3, 64 KiB clusters, 16-bit refcounts, no VM state, and the existing 32 GiB
virtual capacity limit. Physical images are limited to 33 GiB before hashing or
publication, matching the journal capacity ceiling. IDs must be unique and
nonempty; IDs and names are bounded to 256 bytes. At most 64 saved states and one cluster of snapshot-directory bytes
are accepted. A single-cluster L1 table is required for creation. Writable opens
require private ownership of every active L1 allocation cluster, including unused
tail entries. Consistent shared L1 maps remain available to read-only validation
and snapshot views; the writer rejects them before permitting mutation. Deletion
and revert have the bounded contract in
`qcow2-snapshot-lifecycle-design.md`. Renaming, VM-state capture and backing-chain
creation remain unfinished work.

The transaction appends a saved L1 table and a replacement native snapshot
directory. It increases each active L1 reference to an L2 table and each data
reference reachable through that table, including preallocated-zero payloads.
Active copied flags are cleared because the newly saved state shares those
allocations. The prior directory allocation is released. Inactive copied flags
remain irrelevant to ownership, as specified by the [QEMU format](https://www.qemu.org/docs/master/interop/qcow2.html#snapshots).

All changes are staged before mutation. The existing bounded QCOW2 redo journal
validates exact ownership of both original and proposed states, records original
file identity/content, publishes its sidecar without overwriting, and syncs the
parent directory before updating the dirty image. The existing 16-cluster patch
ceiling can reject otherwise valid images before any mutation. Native metadata
and refcount-table relocation are not introduced. Recovery requires reopening the
writer; readers reject pending transactions. Newly appended metadata is synced
before the dirty flag is cleared and the sidecar removed. A recovered operation
may have completed even if its caller received an interruption error; its explicit
ID lets callers inspect completion rather than blindly create a duplicate.

Successful creation refreshes the writer's active descriptors so later payload
writes use existing COW transactions and cannot modify saved bytes. Writable opens
of validated disk-only snapshots preserve the same rule. Snapshot views remain
immutable: all external users must honor locks and avoid concurrent readers during
mutation. Creation does not promise a VM-consistent checkpoint or atomicity for
prior payload writes.
