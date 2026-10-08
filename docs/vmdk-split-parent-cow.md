# Backed split sparse VMDK writes

Linux writers support bounded standalone and authorized backed
`twoGbMaxExtentSparse` descriptors. Backed writes retain read-only parent graph
pins and use the dependency-bound `VDTXSET2` recovery protocol described below.
Uncompressed hosted sparse versions 1 and 2 share the validated mapping layout;
ZERO mappings require the corresponding feature flag. QEMU's
[versioned implementation](https://github.com/qemu/qemu/blob/v10.2.0/block/vmdk.c)
and a small actual native producer fixture establish version-2 ZERO behavior.
Stream-optimized/compressed profiles remain unsupported. Split creation,
publication, native discard and native resize remain separate unfinished work.

The sections below retain the implementation design and its invariants. Scoped
host, interruption and QEMU gates have passed; release-wide checks and native
VMware acceptance are tracked separately in the implementation status.

## Implementation progress

The first scoped integration test, `authorized_split_child_cows_across_extents_and_final_partial_grain`
in `tests/vmdk_split_parent.rs`, compiled and failed as expected: 0 passed,
1 failed. The authorized reader first established the valid inherited fixture;
denied-parent opening preserved the child descriptor, extents and directory
entries. Authorized writer opening then failed with `UnexpectedEof` because
the monolithic resolver attempted a 512-byte header read on the short external
descriptor. This behavioral red does not establish backed writer capability.
Graph resolution, immutable dependency codec and sparse writer glue remain
under implementation; fuzzing stays paused.

A small actual QEMU 10.2.4 producer/import test then recorded a distinct
behavioral red: 0 passed, 1 failed. `qemu-img create` produced a 131072-byte
`twoGbMaxExtentSparse` parent and backed child; `qemu-io` wrote the parent
pattern and a native ZERO grain with `zeroed_grain=on`. Backing-chain inspection
and the initial complete raw conversion succeeded, but this crate rejected the
child as an unsupported sparse profile. A retained header probe showed native
ordinary extents use version 1 / flags 3, while native zeroed-grain extents use
version 2 / flags 7; otherwise their 512-byte headers were identical. This red
is retained while bounded version-2 ZERO-profile validation is investigated;
the native gate is not weakened to avoid that profile.

## Current seams

`vmdk_sparse_set::descriptor` requires `parentCID=ffffffff`. Allocation fills a
new grain with zero before applying the write. `validate_sources` validates the
child descriptor and its writable extent sources as a complete set.

`Vmdk::resolve_writer_parent` supports a hosted monolithic sparse child. Its
`hosted_link` rejects an external split descriptor; it creates a fresh default
budget, starts alias detection with one child identity, and returns a logical
parent without an enumerable physical dependency manifest. The chain reader
already checks authorization, aliases, depth, pending transactions, parent CID
and capacity. Its descriptor reader gives sparse extents bounded parent views.
Those internals are useful to refactor, but calling the existing resolver is
insufficient for the split writer.

`transaction_set` binds participant paths, identities, lengths and digests.
However, every participant has a `RawWriter`, and replay publishes and removes
extent markers even for participants without patches. A `record: None` entry
therefore cannot represent an immutable parent.

## Required dependency and transaction contracts

Keep writable child participants and immutable dependencies distinct in both
memory and the journal. A dependency owns a read-only opened handle, its shared
lock guard, and a bounded manifest entry. Its type exposes no resize, write,
marker publication or sidecar cleanup operation. Only child descriptor and
extents remain writable participants; the descriptor remains participant zero
and its CID remains a durability barrier before extent mapping publication.

The dependency manifest binds every ancestor descriptor and extent, not only
the direct parent's CID. Record canonical path, opened file identity, exact
physical length and SHA-256 digest. Record deterministic node/extent order,
extent kind and logical span, descriptor CID and parentCID, and resolved parent
edges. Validate the resolved hint against the manifest, including its final
published location. A matching CID alone does not establish unchanged parent
bytes. Do not silently resolve a replacement file during recovery.

Count and digest each physical file once. A hosted monolithic parent's embedded
descriptor and sparse extent refer to the same manifest file index; they are
two structural uses of that file, not two independently opened dependencies.
An external descriptor and its split extents use distinct file indices. Reject
unexpected identity aliases between distinct physical entries while preserving
the intentional embedded-descriptor relationship in topology validation.

Use one graph-wide `ReadBudget` for child and ancestor parsing, authorization,
identity comparisons, manifest retention, digest scans, COW reads and both
transaction shadow validations. Bound node count, extent count, path bytes,
depth, physical bytes and serialized manifest size before allocation. Existing
child limits must not accidentally become fresh limits for each ancestor.
The complete graph contains at most 257 physical files, counting the child
descriptor, every writable child extent and every immutable dependency. Its
physical lengths sum to at most 33 GiB; preflight and journal validation use
`max(original_length, final_length)` for writable participants. Paths remain
absolute, nonempty, NUL-free and at most 4096 encoded bytes each; the entire
coordinator codec remains at most 4 MiB, including patches, topology, digests
and checksum. Depth remains bounded by the shared parser limit and 64. These
are ceilings, not promised usable capacity: tighter shared metadata, cache,
work or attribute limits may reject a smaller graph. Count authorization and
path retention against that same budget; do not duplicate reservations through
a fresh per-node budget. Check sums and codec-size arithmetic for overflow
before allocation. This extends the existing set ceilings to dependencies;
it does not relax the existing writable-child profile or capacity ceilings.

Canonicalize and explicitly authorize every parent descriptor and extent.
Compare all opened identities against all child identities and preceding graph
files, rejecting cycles and aliases. Reject pending transactions in parents.
Check parent CID, total capacity, extent capacities, and supported profile rules.
Retain the same opened read-only sources used for parsing and digesting, with
path-to-handle identity and length checks before mutation. Both original and
proposed child views must validate against those retained parent sources.

Acquire shared locks on immutable files and exclusive locks on child files;
keep guards alive for the writer lifetime. Use a deterministic acquisition
order and nonblocking failure to avoid graph lock deadlocks. The configured
Rust 1.99 sysroot exposes `File::try_lock_shared` (stable since 1.89), so no
toolchain or dependency change is needed for that API. Existing `RawDisk`
opens read-only but takes no lock. Add a dedicated retained read-only pin or
private locked constructor and retain its lock-bearing `File` throughout the
logical parent reader and manifest lifetime. Verify read-only locking behavior
on each supported platform with a fixture. Do not open parents writable as a substitute.
Advisory locks only constrain cooperating writers; the caller must retain the
existing external immutability contract against noncooperating modification.

## Codec and recovery migration

Introduce a versioned codec that explicitly distinguishes writable participant
records from immutable dependency entries. Dependencies cannot carry patches,
final lengths, or participant-marker obligations. Validate roles, ordering,
duplicates, topology, bounds and checksums before opening paths or mutating
anything. Decode authorized paths supplied by the caller, never treat journal
paths as authorization.

Before recovery publishes a missing marker, replays a patch, resizes a child,
or removes a sidecar, require exact equality between the journal manifest and
the newly opened authorized dependency graph, including identity, length and
digest. Validate every existing child marker and both complete child shadow
states first. Parent rejection leaves child files and all sidecars unchanged.

Standalone writes and recovery continue using `VDTXSET1` unchanged. Backed
sets use `VDTXSET2`: retain the transaction id and writable participant encoding,
then an explicitly counted immutable dependency section and counted topology
section, covered together by the coordinator checksum. Require a nonempty
dependency section for a backed set. Each dependency stores path, identity,
length and digest; topology refers to bounded manifest indices rather than
duplicating paths. Dependency indices cannot refer to writable participants.
Validate combined counts and physical lengths against the graph limits above.
Never infer parent dependencies missing from a legacy journal, and never
rewrite a pending legacy journal merely to upgrade it. Reject `VDTXSET1` for a
backed child, and reject malformed or inappropriate `VDTXSET2` before cleanup.

Reuse `VDTXPAR1` markers for writable child extents only. They bind the exact
coordinator path and transaction id; they neither encode nor authorize the
coordinator's payload schema. Recovery must read and validate the coordinator's
magic and complete manifest before acting on a matching marker. Old readers
must refuse the new coordinator magic, and ordinary readers must continue
refusing any pending marker. Thus a marker cannot make an old decoder accept
the new format. Prove this with a frozen legacy decoder/recovery fixture rather
than assuming compatibility from the marker bytes. Never create dependency
markers and never delete a parent's preexisting sidecar.

## COW and ZERO semantics

For an unallocated child grain without a ZERO mask, read its inherited content
from logical offset `extent.start + grain * 65536`, bounded by the remaining
extent capacity. Initialize the complete private grain, zero its unused final
tail, then overlay the caller's bytes. Journal the full initialized payload and
its mapping publication, including any newly created grain tables.

An unallocated mapping inherits. A ZERO mapping returns zero even when the
parent contains data. Allocating from a ZERO mapping initializes with zero and
must not copy parent bytes. Partial zeroing preserves surrounding inherited
bytes through full-grain COW. Supported whole-grain ZERO publication must update
primary and redundant metadata together through recovery. Discard must never
remove a child mask and expose parent bytes.

Whole-call preflight must cover every touched extent, missing table, projected
length, scratch reservation, inherited read and validation/digest cost before
the first CID change. Parent dependencies remain immutable through success,
failure, interruption and reopen.

## API seams and failure behavior

Refactor graph resolution to accept a parsed child link, caller-owned budget and
all child identities. Return the logical parent plus the retained physical
manifest and locked sources. Add a split-descriptor parent-aware validation
path using supplied child shadow sources; do not reopen mutable child paths to
validate recovery states. Extend `Sparse` with the resolved parent and immutable
dependencies, and make read/allocation paths honor ZERO and inheritance.

Introduce an internal source factory passed through `chain_node` and
`parse_descriptor`. Its initial graph-open mode canonicalizes and authorizes a
requested descriptor or extent, opens it read-only, acquires a nonblocking
shared lock, checks identity against the complete child/ancestor identity set,
and stores a manifest-indexed retained source. Charge each action to the caller's
budget. Factory output includes canonical binding, source and opened identity;
retain ownership of the lock-bearing handle in the graph object. An ordinary
read-only chain open can retain its existing policy through a separate factory
implementation; writer graph resolution must use the pinned implementation.

After discovery, freeze the manifest. Its validation factory can only return
already retained sources by exact canonical binding and expected role/index;
an unknown request fails and cannot trigger an open. Route parent-node and
parent-extent parsing through this factory. For each old/new child shadow
validation, supply the descriptor and writable extent shadow sources through a
separate child lookup and supply the same frozen dependency factory for parent
requests. Resolve hint/extent path bindings against that manifest and check
current path metadata against opened identity and length before replay; path
checks must not replace a retained source. Reuse the immutable logical parent
only after its linkage and manifest are validated, or reparse retained sources
under the same budget. Both alternatives prohibit dependency reopening.

Keep standalone opens strict. A backed open requires explicit authority for
both writable child extents and immutable parent graph paths. Unsupported
profiles, denied authority, invalid linkage, aliases, changed dependencies,
lock conflicts and budget exhaustion fail before recovery mutation. A failed
write retains the existing poisoned-handle/reopen contract where applicable;
reopen performs the complete dependency validation again.

## TDD red ladder

1. Start with two child sparse extents over an authorized parent containing
   different nonzero patterns in each logical region. An inherited read fails
   through the current writer open restriction. Assert unauthorized parent and
   extent opens fail without modifying any file or sidecar.
2. Write a few bytes across the child extent boundary into unallocated grains.
   Check private bytes, inherited surroundings, byte-identical parent files and
   unchanged sibling reads. Include a final partial grain.
3. Add a ZERO-masked grain with a nonzero parent; verify reads and subsequent
   partial writes keep the surrounding bytes zero. Verify partial zeroing keeps
   inherited surroundings and full-grain zeroing masks the parent.
4. Exercise missing primary/redundant grain tables, multiple touched tables and
   whole-call preflight rejection. Add graph aliases, cycles, CID/capacity
   mismatches, deeper chains and cumulative budget exhaustion.
5. Interrupt at every publication, resize, payload, mapping, sync and cleanup
   barrier. Recover only with the exact authorized parent graph. Independently
   change a descriptor, same-length extent bytes while preserving CID, extent
   length, path identity and authorization. Each rejection preserves all child
   and sidecar bytes. Include an unchanged read-only parent on a filesystem
   where writable opening is denied.
6. Test new codec role corruption and bounds, standalone legacy recovery, and
   rejection of a backed operation with an unbound legacy journal.
   Cover the combined 257-file/33-GiB ceilings, codec overflow, graph-wide path
   charges and a tighter caller budget. Feed a complete `VDTXSET2` journal to a
   frozen `VDTXSET1` decoder and require refusal. Feed it to frozen legacy
   recovery with matching `VDTXPAR1` markers and require unchanged coordinator,
   markers and image bytes. Verify an ordinary reader refuses a child extent
   carrying such a marker. Verify new recovery rejects a mismatched marker id
   or coordinator path before any mutation. Instrument the frozen validation
   source factory to fail on any filesystem open and show both child shadow
   states validate entirely from retained sources.

## Native QEMU gates

Use independently created `twoGbMaxExtentSparse` backed images and record QEMU
version and commands. Verify `qemu-img info --backing-chain`, raw conversion or
logical comparison, cross-extent private COW, ZERO masks, final partial grains,
missing-table allocation and recovered output at every interruption cut.
Hash all parent files and compare sibling logical content before and after.
Validate backing references after child staging/publication in another
directory. Native tool acceptance is a separate gate from synthetic fixtures;
supported command/options and QEMU ZERO behavior must be checked experimentally.
These checks establish VMDK interoperability, not Windows servicing or capture
correctness. Fuzzing remains outside this proposed work while explicitly paused.
