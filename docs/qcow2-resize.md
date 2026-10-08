# Native QCOW2 capacity transactions

`Qcow2Writer::resize(&mut self, new_size, ShrinkPolicy)` changes the existing
container's virtual capacity, retaining its opened exclusive file lock. The
supported profile is Linux, standalone or explicitly authorized backed QCOW2
v3, 64 KiB clusters,
16-bit refcounts, sector-aligned capacity at most 32 GiB. Internal snapshots are supported within
the native lifecycle bounds: at most 64 snapshots, a one-cluster snapshot
directory and active L1 table, and physical container size at most 33 GiB.
Parents remain immutable and must be explicitly authorized at opening and
recovery. The caller must exclude every external dependent
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

Internal snapshots retain their original logical bytes and saved virtual
capacity across active growth and shrink. `RequireZero` checks the active tail;
`AllowDataLoss` affects the active image only. Reverting a saved state restores
its saved capacity, including zero-capacity snapshots. The supported v3 parser
requires the saved-size field; legacy read-only v2 fallback does not extend the
writable profile. A full snapshot directory can still be resized because resize
does not add directory entries.

Behavioral tests cover active/private/zero bytes, saved states of different
capacities, empty images, CLI use and mutation-free zero-tail refusal. Recovery
tests cover every changed cluster in the growth/shrink fixtures and persistence
cuts, checking complete active and saved streams after reopen. An independent
QEMU oracle checks ownership and converts both active images and restored copies
of their snapshots. It uses `qemu-img snapshot -a` on a copy before conversion:
[QEMU's temporary snapshot loader](https://github.com/qemu/qemu/blob/master/block/qcow2-snapshot.c)
switches mappings without restoring capacity, while snapshot restoration also
restores the saved size. The original source remains byte-for-byte unchanged.
These tests do not establish actual power-loss or native Windows correctness.


Backed growth writes explicit zero descriptors for new whole clusters and a
private boundary payload when the old capacity ends in an inherited cluster.
This prevents nonzero parent bytes from appearing in new space. Shrink's
`RequireZero` policy checks resolved inherited bytes as well as private payloads;
explicit zero mappings and bytes beyond the parent's capacity resolve to zero.
For an inherited partial cluster, shrink copies the retained prefix and clears
the suffix so later growth cannot reveal discarded parent bytes. Parents and
saved snapshot maps remain unchanged. All masks, boundary data, ownership and
capacity changes share one transaction; requests exceeding its patch budget
fail before mutation. Large backed growth may therefore be unsupported even
within the virtual capacity limit.

Tests cover raw and QCOW2 parents, nonzero inherited-tail refusal, mutation-free
patch-budget refusal, zero-tail shrink, growth beyond parent capacity, shrink and
regrowth, saved inheritance and parent preservation. Recovery checks both parent
formats for every fixture patch and persistence cut; unauthorized recovery is
refused without changing the interrupted child. The independent QEMU check covers
complete active and restored saved streams and ownership for both parent formats
and both resize directions.
