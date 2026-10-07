# QCOW2 sparse write recovery

## Scope and contract

The next writable profile is standalone QCOW2 v3, 64 KiB clusters, 16-bit
refcounts, no compression, encryption, snapshots or unknown
metadata extensions. Backing chains are permitted only through explicit
`Qcow2Writer::open_chain` authorization, using the same canonical path, parser
budget, recursion and file-identity cycle policy as the read-only chain loader. It accepts sparse mappings and privately owned or shared
uncompressed data. Shared payloads and L2 tables are copied before modification. Cloning an L2
table preserves reachable payload reference counts because the original L1
reference is moved, not duplicated. Decrementing a shared L2 table or payload
to one reference repairs copied flags on the remaining L1/L2 references. All
repairs are included in the same validated journal transaction. A partial write into an
unallocated overlay cluster first copies the corresponding authorized parent
cluster, zero-padding beyond parent EOF. Explicit zero mappings suppress parent
inheritance, including during later partial writes. Guest zero
mappings must remain zero. Existing fully allocated images stay supported.

An exclusive image lock is held during validation, transactions and recovery.
Linux journaled writers require exactly one hard link on the opened image, checked
on opening and before allocation. This prevents a clean image with a published
journal being reopened through an alternate hard-link name before dirty marking.
Noncooperating creation of new aliases during a transaction violates external
exclusion. Windows allocation remains unsupported until an equivalent identity
and persistence policy is tested.
All external tools must respect the lock and must not mutate the image or its
journal. Guest writes can span multiple transactions and can partially complete;
only a committed individual allocation transaction has a recoverable outcome.
`flush` is the durability boundary for ordinary private payload writes. Metadata
allocation transactions persist their journal and image before reporting success.

## Chosen recovery mechanism

Use a library-owned sidecar redo journal plus QCOW2's native dirty incompatible
feature. Do not claim the native dirty flag itself provides a transaction or that
QEMU can replay this sidecar. The dirty flag does not guarantee that external tools refuse I/O: QEMU may
read or repair such images. Explicit external exclusion is required throughout
mutation and recovery. This library rejects journal-free dirty images.

An image with a present sidecar is recovered before normal reading or writing.
Backed-child recovery validates original and proposed views using caller-authorized
immutable raw/QCOW2 parents. Opening without those authorizations fails before
image mutation. Parents remain read-only and must be immutable through recovery;
this library does not provide an application-consistent parent snapshot. Child
validation reads through the already locked handle, never a second child file open.
Read-only opening must either build a validated logical recovered view without
mutating either file or return an explicit recovery-required error. It must never
expose a partially updated image as clean.

The sidecar is a versioned binary record of one transaction, with bounded counts,
checked offsets, original and final lengths, old and replacement bytes for every
modified range, a cryptographic checksum of the record, and a cryptographic digest
of the original image. Hashing and image comparison use fixed-size buffers.

Before replay, reconstruct the original logical image by substituting journal old
bytes into changed ranges and ignoring appended bytes. Its digest must match the
record. Normalize the native dirty marker to the recorded original clean header
while reconstructing: an intermediate dirty header differs from both the original
clean header and the final clean header. No other feature bits are normalized. Every currently changed range must contain only bytes justified by the old
or replacement range; torn writes are recoverable because full replacement bytes
are retained. Validate the reconstructed original QCOW2 mapping and refcounts,
then validate the proposed post-transaction image and exact allocation ownership.
Reject foreign, truncated, overlapping, out-of-budget or corrupt records without
mutating either image or journal. A content-identical copied image is a permissible
recovery target: identity is the original content, not a pathname or inode.

The journal filename is derived from the image filename, opened with no symlink
following where supported, and created with `create_new`. An existing unrelated
file must never be overwritten. Its parent-directory durability is required before
mutating the image. Platform directory syncing and atomic journal publication must
be implemented and tested explicitly; unsupported platforms fail before mutation.

## Transaction ordering

1. Lock the image; recover any previous transaction; validate the writable profile.
2. Compute old/new payload, L2, L1, refcount blocks and refcount-table ranges. New
   physical clusters append to EOF initially; free-cluster reuse is deferred.
3. Produce and validate the complete post-transaction logical image using a bounded
   overlay reader. Check all metadata/data ownership, integer ranges and limits.
4. Write the complete journal to a newly created temporary sidecar; sync the record;
   atomically publish the canonical sidecar; sync its parent directory.
5. Set and sync the image's native dirty bit before any allocation metadata changes.
6. Extend the image and write payload and new metadata; update refcounts before
   publishing guest mappings. Write existing metadata ranges last. Sync the image.
7. Clear the dirty bit and sync the clean image. Retain the journal until this succeeds.
8. Remove the journal and sync the parent directory. A surviving valid journal is
   idempotently replayed even if the image is already clean. An alternate hard-link
   pathname can miss the sidecar: dirty image opening must fail closed. A clean
   image without that pathname-specific journal is safe only after the clean sync
   barrier, when the complete transaction is already committed.

Recovery locks the image, validates both reconstructed states, sets/syncs dirty,
replays replacement ranges, syncs, clears/syncs dirty and durably removes the journal.
A journal with an image whose original reconstruction fails validation is not replayed.

## Initial bounds

- Virtual capacity: 32 GiB, sector aligned.
- Cluster and refcount widths: 64 KiB and 16 bits.
- Single transaction: one guest cluster, at most 16 changed physical clusters.
- Journal: at most 4 MiB including old/new bytes and record framing.
- Memory: bounded journal plus existing strict parser budgets; no guest-sized buffers.
- Mapping/refcount ownership: existing strict validation limits remain authoritative.
- Refcount-table growth beyond the existing table coverage fails before mutation
  until transactional refcount-table relocation is implemented.

Transaction errors distinguish unsupported profile, recovery required, journal
corruption/identity mismatch, resource limits and ordinary I/O errors. No automatic
journal deletion on an error. Interrupted payload-only writes remain partial guest
writes; interrupted allocation publication is recovered through the retained record.

## Test-driven implementation gates

1. Sparse QEMU-created fixture opens writable; writing an unallocated cluster allocates
   valid private payload and reference counts, with unchanged adjacent zero reads.
2. L1/L2 allocation and shared payload COW preserve all prior logical bytes.
3. Zero mappings, partial-cluster writes and cross-cluster writes match a memory model.
4. Inject interruption after every journal, dirty-bit, length, payload, refcount,
   mapping, sync, clean-bit and journal-removal boundary. Reopen recovers one valid
   transaction outcome and passes independent `qemu-img check` and raw conversion.
5. Inject short/torn range writes, journal truncation, altered checksum, wrong-image
   journals, offset overflow, overlapping descriptors and excessive limits.
6. Repeated recovery is idempotent; replay failure retains a recovery-required state.
7. Cooperative lock contention prevents concurrent mutation; no second image handle
   is used for reading while a Windows exclusive file lock is held.
8. Verify Linux and Windows runtime persistence separately. Host test success does
   not establish Windows filesystem, servicing or capture correctness.

Do not enable sparse allocation writes until these gates and directory persistence
are implemented. The existing private payload writer remains useful during this work.

## Native discard

`Qcow2Writer::discard(offset, length)` operates on complete 64 KiB guest clusters.
Every nonempty range is checked for capacity and alignment before any mutation.
The final partial guest cluster must use explicit zero writes or a caller-selected
higher-level fallback; it is not silently rounded outward. An empty bounded range
is a no-op. Capacity remains unchanged.

Each discarded cluster uses one durable transaction to release any prior payload
reference and store a zero L2 mapping. The zero flag suppresses parent inheritance,
including after later partial writes. Shared L2 metadata is copied first, with
remaining copied flags repaired, so discarding one alias cannot affect another.
Unallocated inherited clusters may need new L2 metadata to represent the mask.
Physical host holes and EOF truncation are separate operations, not promised by
container reference-count release. Multi-cluster discard can partially complete;
reopen is required after an interrupted transaction.

The shared-L2 fixture is validated with independent QEMU checking before and after
COW; shared/discard fault tests cover dirty barriers and every changed-cluster
write. The format reference is [QEMU QCOW2 specification](https://www.qemu.org/docs/master/interop/qcow2.html).

Native standalone capacity changes use the same journal and opened-file identity
contract. Header capacity/L1 coverage, removed ownership and boundary payload COW
are committed together. `Qcow2Writer::resize` requires exclusive mutable access,
checks `ShrinkPolicy`, rejects backed images, and preserves zero reads on regrowth.
See [native resize design](qcow2-resize.md) for profile and dependency limits.
