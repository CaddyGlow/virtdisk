# Hosted image metadata recovery

VDI sparse allocation uses a bounded external redo journal. The native VDI format
has no allocation transaction log. The journal is required for library reads while
an allocation is pending; external tools must not open the image until recovery
completes. Native VDI identifiers do not substitute for transaction durability.

## Transaction contract

The writer retains an exclusive lock on the opened regular image. Journaled
allocation initially requires Linux directory synchronization and exactly one hard
link, checked on the opened file before every transaction. The path is canonical;
symlink names therefore share a journal. Callers must exclude external replacement,
new hard links and readers/writers which ignore advisory locks.

The sidecar `.virtdisk-transaction` contains a journal version tag, original and final file
length, original SHA-256 content identity, sorted nonoverlapping old/new patches with a validated persisted replay order,
and its own SHA-256 checksum. The initial implementation caps the journal at 4 MiB,
16 patches, and 1 MiB per patch. It rejects malformed lengths, overlap, truncation,
foreign source bytes, and invalid original or proposed container mappings before
any image mutation. The hash reconstructs original patched ranges so interrupted
metadata writes remain recoverable. Unpatched bytes remain part of the identity.

1. Persist a fresh VDI modification UUID before changing user-visible content.
2. Build and validate the complete old/proposed image views.
3. Create a unique temporary sidecar, write it, and synchronize it.
4. Publish the journal without overwriting an existing sidecar and sync its directory.
5. Extend the image and apply redo patches, including zero-padded new payload,
   allocation map and allocated-block count.
6. Sync the image before removing the journal; sync its directory after removal.

Opening a writer with a journal validates and repeats the redo operation under its
exclusive lock. Recovery is idempotent. Opening a reader with a pending journal
fails closed. Any failed allocation poisons the current writer until reopening;
subsequent reads or writes must not observe partially updated in-memory mappings.
Existing allocated payload writes remain non-atomic and can partially complete.

## Initial VDI scope

Standalone VDI 1.1 fixed and dynamic profiles, no per-block prefix, positive
sector-aligned capacity no larger than 32 GiB, 1 MiB blocks for creation. Sparse
allocation appends exactly one zero-initialized block and publishes one mapping.
Free and explicit-zero mappings read as zero. Zeroing an unallocated range does
not allocate it. Unknown trailing physical data is rejected for allocation until
ownership is defined. Parent images, snapshot mutation, resize and discard are
outside this allocation transaction's current scope.

## Validation

Test each durable interruption boundary, metadata patch boundaries and reopening;
reject foreign journals, aliases, malformed records and overlapping patches.
Compare reopened logical data and allocation maps with a reference byte model.
Use QEMU as an independent readback/check oracle after real sparse writes. Host
fault injection establishes the specified syscall recovery contract, not Windows
servicing/capture correctness or physical-media power-loss behavior.

## VMDK hosted sparse allocation

VMDK writable sparse allocation uses the same sidecar contract for standalone
hosted sparse v1 images with 64 KiB grains and 512 entries per grain table. The
embedded monolithic descriptor must carry a valid CID. A new CID is durably
installed before the first user-visible modification after opening or flushing;
external children must therefore not silently match a modified parent.

An allocation appends one zero-padded private grain. Existing primary and
redundant grain table entries are updated in the same journal transaction. Both
old and proposed mapping views must pass bounds, ownership and redundant-map
agreement checks. Missing grain tables initially fail closed before any mutation;
creation preallocates the required tables so all unallocated grains can be written.
Transaction patch ordering is payload, redundant mapping, primary mapping, with
an image sync before sidecar removal. Journaling makes interrupted orderings
recoverable without relying on a torn native mapping or the native dirty flag.

Pending readers reject the sidecar; writers redo it under lock. Linux writable
opens require one hard link, including fully allocated opens, so a clean pending
journal cannot be bypassed through another file name. External tools must remain
excluded until sidecar recovery completes. Native snapshot/parent mutations,
compressed grains, descriptor multi-file writes and resize remain outside this
initial sparse writable profile.

The version-2 sidecar stores replay order independently of file offset. Recovery
checks that it is a permutation of all patch indexes, so metadata topology never
implicitly chooses persistence ordering. Existing shorter hexadecimal CID fields
retain their field width; newly exported images use eight hexadecimal digits.

## Authorized VMDK parent chains and hosted overlays

`Vmdk::open_chain` resolves embedded hosted sparse and external flat/split sparse
VMDK descriptors. Every ancestor and external extent requires explicit canonical
path authorization. Actual opened file identities reject hard-link cycles and
extent aliases; CID linkage is checked numerically, including short hexadecimal
CID fields produced by QEMU. A parent hint without a parent CID, duplicate linkage
properties, protocol/device syntax, capacity mismatches and unresolved parents
fail closed. One shared parser budget covers the chain; depth is bounded by the
caller limit and a 64-image hard ceiling. Readers and ancestors must stay immutable.

Free grain entries inherit the corresponding parent range. Explicit-zero entries
mask that range. Flat extents always supply their own bytes. External split sparse
extents inherit translated slices of the descriptor's parent, so split boundaries
cannot restart the parent offset. This profile requires equal parent/child capacity.

`VmdkWriter::create_overlay` creates a new hosted sparse child with an embedded
parent CID and hint, and `open_chain` locks and recovers an existing hosted child.
Writable child capacity and geometry retain the existing 32 GiB/64 KiB/512-entry
limits. Multi-file child writers and missing grain-table creation remain unsupported.
The parent can itself use supported external descriptors and authorized chains.
Linux single-link exclusion and the existing journal durability contract apply.

For a free inherited grain, allocation first reads the complete visible parent
grain into bounded scratch, padding the last partial grain with zero. It applies
the requested write or zero range to that private payload before publishing the
mapping. Replay order remains payload, redundant GTE, primary GTE. The journal
stores the resulting payload, so replay never rereads parent payload to reconstruct
an interrupted write. Explicit-zero grains start from zero instead of inherited
bytes. Existing allocated payload writes retain their documented non-atomic contract.
A fresh child CID is synced before modification; parent files are never modified.
Ancestors and their extents must remain externally immutable while a child is open.

Primary linkage reference: [Broadcom parent virtual disk troubleshooting](https://knowledge.broadcom.com/external/article/319681/troubleshooting-parent-virtual-disk-erro.html).
The independent oracle is [QEMU's VMDK driver](https://github.com/qemu/qemu/blob/master/block/vmdk.c),
with QEMU-created backing chains and QEMU conversion/readback of native overlays.

## Existing flat descriptor writes

`VmdkWriter::open_descriptor` locks an existing standalone `monolithicFlat` or
`twoGbMaxExtentFlat` descriptor and every explicitly authorized `RW FLAT`
regular-file extent. Monolithic descriptors require exactly one extent; split
descriptors allow up to 256 nonempty extents, each no larger than 2 GiB.
Descriptor parsing is limited to 64 KiB, authorization to 256 paths and virtual
capacity to 32 GiB. It checks the sector offset and full extent coverage before
mutation. All actual opened-file identities remain retained with their exclusive
locks; aliases and pending transaction sidecars fail closed. Linux opens also
require one hard link for each file. Parented flat writers,
creation and resizing remain unsupported.

Reads and payload writes translate only into declared existing extent slices;
prefix and suffix bytes remain outside the virtual disk. A new numeric descriptor
CID is synced before the first nonempty mutation in each open/flush epoch, then
payload writes or zeroes occur under the writer mutex. Cross-boundary operations
visit extents in virtual order without allocating a disk-sized buffer. Flush syncs
every extent followed by the descriptor. Payload writes are not atomic: an I/O
error can leave an already completed prefix, including earlier extents, modified.
Flush errors can likewise follow successful synchronization of earlier extents;
this profile introduces no multi-file journal or publication transaction. All
external users must honor the locks and immutable-reader contract.

Independent fixtures are generated by QEMU with `subformat=monolithicFlat` and
`subformat=twoGbMaxExtentFlat`, then converted back to raw after positional writes
and zeroing. The split fixture exceeds 2 GiB and exercises a native extent
boundary. Tests additionally verify retained locks, explicit authorization,
offset-slice boundaries, aliases, concurrent writes, CID freshness, failed-open
lock release and generic writer inspection. Creation of multi-file images remains
deferred until an atomic multi-file publication protocol is specified.
