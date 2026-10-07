# Native hosted sparse VMDK discard

`VmdkWriter::discard(offset, length, DiscardPolicy)` guarantees zero-readable
logical bytes without changing capacity. On Linux, hosted sparse standalone
images and explicitly authorized overlays support native discard of complete
64 KiB grains, including a final grain clipped to the virtual capacity. The
physical image must fit the existing journal's 33 GiB container ceiling. Flat
and split descriptors and partial grains require `AllowZeroFallback`; strict
requests return `Unsupported` before any CID or payload mutation. Invalid
ranges fail before processing a prefix. Empty ranges return `Zeroed`.

Native discard publishes ZERO grain entries rather than unallocated entries.
This distinction masks inherited parent bytes. The zero-grain header feature
is enabled before publishing redundant and primary mappings, using one bounded
sidecar transaction per grain. A fresh CID is synced before the first mutation
in an epoch. Locks stay held; saved parents remain immutable. Repeating an
already masked grain changes neither CID nor container bytes.

Live mappings and zero masks share the existing writer mutex. A partial write
after discard allocates a zero-filled private grain, preserving zero neighbors
instead of copying the parent. Its zero-mask state is cleared after successful
allocation, allowing subsequent discard to release the new mapping. This state
also remains consistent through native capacity changes and reopening.

`Deallocated` means the native container accepted the mapping release and zero
mask. It does not promise host hole punching, truncation, secure erasure or a
smaller allocated file. Unreferenced physical payload bytes can remain. The
existing verified new-output compaction operation can remove such storage
separately. `Zeroed` denotes the explicit fallback with no reclamation promise.

Every journal validates the original and proposed supported allocation owners,
including authorized parent resolution. Failure after publishing a sidecar
poisons the live writer and requires reopening for recovery. Completed grains
can remain as a zeroed prefix when a later grain fails. Cancellation and real
storage power-loss behavior are not established by simulated interruption
tests. Physical-image hashing makes discard of large allocated files potentially
expensive.

Regression tests cover allocated, absent and inherited grains, final clipped
capacity, repeated discard after COW writes, unchanged invalid/strict requests,
fallback neighbors, descriptor rejection and idempotence. Fault tests cover
primary-only and redundant mappings with the zero feature initially enabled or
disabled, on standalone and parented images. Native QEMU fixtures are checked
and converted for exact bytes after normal mutations and every redo boundary.
These tests establish native container interoperability with QEMU, not native
VMware runtime acceptance.
