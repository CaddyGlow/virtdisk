# Native QCOW2 capacity transactions

`Qcow2Writer::resize(&mut self, new_size, ShrinkPolicy)` changes the existing
container's virtual capacity, retaining its opened exclusive file lock. The
initial supported profile is Linux, standalone QCOW2 v3, 64 KiB clusters,
16-bit refcounts, sector-aligned capacity at most 32 GiB, no internal snapshots.
Backed images are rejected. The caller must exclude every external dependent
snapshot and all concurrent image access; native metadata cannot discover them.

Shrink rejects by default, verifies the entire removed logical tail for
`RequireZero`, or accepts data loss only with `AllowDataLoss`. Growth reads zero.
For a retained boundary payload, a private copy clears its invisible suffix;
subsequent growth cannot expose truncated bytes. Shrink releases removed payload
and L2 ownership and clears their active descriptors. Shared L2/payload mappings
use COW before changing retained mappings. L1 coverage grows within a bounded
single cluster (32 GiB needs 64 entries); zero-capacity images allocate an L1
cluster when first grown. Existing excess L1 coverage is retained with zero
entries. Existing refcount capacity is used; relocation remains unsupported.

All changes form one existing bounded redo journal transaction: prepare without
writes, verify original/proposed strict ownership, publish+sync journal, dirty
marker+sync, append if needed, apply patches, sync image, clear+sync dirty,
remove+sync journal. Reopen replays the whole capacity change after interruption.
The old/proposed header sizes are part of identity validation; only the native
transient dirty bit is normalized. A failed writer requires reopen. Transactions
above 16 changed clusters are rejected before mutation. No host hole punching or
physical tail truncation is promised. The journal cannot promise underlying
hardware correctness beyond host sync guarantees. External QEMU access remains
excluded even while the dirty flag is set.
