# Split sparse VMDK transactions

Linux standalone `twoGbMaxExtentSparse` descriptors retain all child locks and support writes, zeroing and bounded grain allocation through a complete-set coordinator. Allocation supports holes and native ZERO mappings when primary and required redundant grain tables already exist and the target physical EOF is 64 KiB aligned. Cross-grain I/O failures can complete a prefix. Bounded missing-table creation is supported through an appended metadata arena in an entirely unallocated extent or through validated unclaimed metadata padding below unchanged overhead in an allocated extent. Complete-call plans reserve all required pairs within the bounded geometry and remaining resource limits. Backed COW, native discard, creation and resize remain planned and unsupported.

See [the implementation plan](implementation-plan.md), [current status](implementation-status.md), [standalone discard](vmdk-discard.md) and [standalone resize](vmdk-resize.md) for existing scope.

## Reuse and separation

`src/vmdk_flat.rs` provides retained descriptor/extent locks, explicit authorization, opened identity alias checks, and bounded descriptor discovery. `src/vmdk.rs::parse_descriptor` reads split sparse extents with one shared parser budget and appropriately offset parent slices. It already rejects pending transaction sidecars on descriptors and extents.

`src/transaction.rs` provides bounded patches, checksums, reconstructed original digests, mixed old/new patch validation, old/proposed shadow readers, no-overwrite publication and Linux directory synchronization. Extract narrowly reusable record validation and shadow construction while preserving its existing single-file format and behavior. Do not recover a set by looping over single-file `recover`: that can mutate an earlier participant before discovering a foreign later participant.

Add `src/vmdk_sparse_set.rs` for the retained sparse extent backend and `src/transaction_set.rs` for the coordinator. Reuse sparse ownership/mapping validation from `src/vmdk_writer.rs` without opening independent component writers or publishing independent component CID epochs.

## Historical first bounded profile

The profile uses Linux directory durability, a descriptor of at most 64 KiB, at most 256 explicitly authorized extents, logical capacity at most 32 GiB, 64 KiB grains and 512-entry grain tables. Each split extent has at most 2 GiB logical capacity, with an embedded descriptor reservation of at most 1 MiB. Physical ownership, redundant mapping agreement, grain alignment and actual physical-length bounds must also pass. Reject mixed, stream optimized and unsupported geometries unchanged. Missing tables remain readable as holes, but a write needing table creation refuses for the complete request before mutation.

Lock the descriptor, discover its bounded extent set, then retain locks on every child extent before accepting the writer. Compare opened identities, not just canonical paths, against the descriptor, every other extent and every authorized parent file. Require the single-link journal precondition for writable participants. Parent extents remain immutable readers and require explicit authorization.

All descriptor, sparse ownership, old/proposed shadow validation, parent traversal and coordinator work shares cumulative limits. Per-file validation limits are not independently replenished. Cache reservations cover retained extent metadata and scratch views before publication.

## Coordinator format and practicality

Use a new checksummed, versioned set format rather than concatenating up to 256 existing 4 MiB records. A bounded header contains transaction ID, profile, logical capacity and descriptor identity. An ordered participant table contains descriptor and every child extent, canonical paths, Linux device/inode identities, original/final physical lengths, and reconstructed original SHA-256 digests. Encode paths with a bounded length and a round-trippable Linux byte representation; reject ambiguous or duplicate entries.

Every child is included, even if this grain transaction changes only one extent. An unchanged participant has equal original/final lengths, a digest and zero patches. Do not weaken this to identity and length alone: an in-place modification preserves both. Recovery verifies the full unchanged digest before any mutation. Changed participants reconstruct their original bytes from current mixed old/new patch contents and compare the reconstructed digest.

The complete coordinator is capped at 4 MiB, all patches together at 16, individual old/new patch spans at 1 MiB, and transaction payload at one grain. Marker path length is bounded to 4096 bytes; there are at most 256 markers. Participant tables count toward the coordinator bound. Reject exceeding any bound before CID or payload changes. No shrink/tail archive is needed in this increment. Future allocation must keep length growth and sector entry encoding overflow checks.

This intentionally scans all participating physical bytes per transaction, including unchanged extents, and may be expensive. Both original and projected final complete physical sets are capped at 33 GiB; digest scans charge fixed 64 KiB work chunks to the retained shared parser budget. Both original/proposed parser states and repeated replay validation consume that same budget. Before a whole write or zero call, project every distinct missing grain's append, check table slots, aligned tails and sector encoding, then check the final aggregate and remaining cumulative work, metadata and cache capacity. A later unsupported extent, exhausted budget or projected overflow leaves the complete call unchanged.

Existing-table allocation leaves geometry and table/descriptor metadata sizes unchanged. Parser work already covers all logical grains and existing table entries, including holes, and parser cache reserves all possible grain ownership intervals. Preflight additionally allows conservative ownership work for all logical grains on each of the four original/proposed validation passes, five complete scans at the projected final lengths, journal/cache bounds and doubled grain segmentation for zeroing chunks. Cached mappings/masks and table-slot arrays reserve storage for every logical grain before publication. Counters are cumulative for the opened writer; reopening starts a new bounded operation. Do not silently bypass digests for large sets. Merkle indexes or persistent sparse-aware identity schemes require a later independently validated design.

The implemented first increment refuses parents. Before adding backed COW, bind the authorized parent set and linkage to the transaction. A conservative backed implementation should pin parent identities, lengths and full digests and verify them before recovery; parent hash scanning also consumes the shared work budget. The caller must exclude external parent mutation because advisory locks cannot establish universal immutability.

The validator accepts the whole collection of shadow sources, with an explicitly authorized resolver and one shared budget. It validates descriptor linkage and the complete original and proposed logical image states. It must not reopen path-based image bytes instead of the supplied shadows.

## Publication and recovery

1. Acquire and retain all participant locks; validate identity, authority, limits, old/proposed whole-image states and directory persistence before mutation.
2. Publish the coordinator at the descriptor's `.virtdisk-transaction` path with no overwrite, synchronize its file and containing directory.
3. Publish bounded extent markers at the existing transaction sidecar paths. Each identifies its coordinator and transaction ID. Synchronize every marker directory before any image mutation.
4. Publish the coordinator's fresh descriptor CID and synchronize it before payload mutation. Then write payload before redundant and primary mapping publication. Global patch ordering must express the CID durability barrier, not merely its position in a write loop.
5. Synchronize every changed participant before removing markers. Remove and synchronize extent markers, then remove and synchronize the coordinator last. Retain locks throughout.
6. On an error after publication, poison the current writer and require reopening for recovery.

An extent marker must be recognized by standalone writer recovery as a set participant and produce an explicit refusal to recover it independently. Ordinary readers already conservatively reject sidecar existence. No native operation may advertise writable split support before this behavior exists.

Coordinator publication precedes markers; an interruption during marker publication has not mutated images. A missing marker can therefore be legitimate. On recovery, acquire the complete authorized lock set, check all participant identities, reconstructed original digests, patch bytes and complete old/proposed states before the first write. Republish missing owned markers and synchronize all directories before replay. Missing markers after completed publication can also occur during cleanup; validated replay is idempotent.

Reject foreign, malformed, symlinked or mismatched markers without replacing or deleting them. Read sidecars with no-follow and nonblocking flags, then require regular files; a FIFO cannot block recovery waiting for a writer. Before removing any marker, verify its transaction ID and coordinator binding; an existence check alone does not establish ownership. Directory/path checks verify current canonical paths still identify the retained opened files and actual file lengths agree with retained writer lengths before mutation or cleanup. Coordinate these checks with the retained locks and document the residual threat from actors that ignore locks and change directories or image contents.

During cleanup, an extent whose marker has been removed exposes a fully synchronized final state. The descriptor coordinator remains pending until cleanup completes. Direct readers must never see an unmarked extent containing a partially published transaction.

## CID, COW and discard

The existing `begin_mutation` synchronizes a CID independently of its grain journal. The split backend owns its descriptor CID publication in the coordinator protocol instead. Generate one fresh CID for the writer's first mutation and include it in the first transaction, with a durability barrier before payload changes. Later grain transactions keep that epoch; reopening establishes another fresh mutation epoch.

Commit one grain at a time. Cross-extent calls may complete a documented prefix on I/O failure; they do not promise whole-request atomicity. Missing standalone grains receive a complete zero-initialized 64 KiB payload with requested bytes applied, followed by redundant and primary GTE publication. Native ZERO input mappings also start from zeros; orphaned former payload bytes cannot reappear. Mapping/mask caches are mutable under the writer's operation lock and update only after successful commit. Failed transactions poison the writer until reopening rebuilds validated caches.

Future backed grain COW must read the appropriate immutable parent slice and preserve all bytes outside the requested range. Future native full or clipped-final grain discard must install ZERO mappings, including the required flag and redundant mapping updates, without revealing parent data. Partial rewrite after discard must start from zeros.

## Test-driven stages

1. Add codec, aggregate-bound, marker ownership and whole-set shadow validation tests without enabling public split writes. Prove that replacing the last participant prevents any mutation of the first.
2. Add retained writable split open/read tests: authorization failures, busy final extent, released earlier locks, aliases, malformed geometry and cumulative limits. Continue to refuse mutation.
3. Add journaled overwrites of allocated grains and coordinated CID publication. Test cross-boundary exact bytes and every publication/replay/cleanup cut.
4. Implemented standalone sparse allocation with payload-before-redundant-before-primary publication, complete-call projected preflight and reopened ownership checks. Table creation remains unsupported.
5. Add explicitly authorized parented split COW, exact inherited neighbors, immutable parent hashes and parent linkage changes rejected before recovery writes.
6. Add full/clipped grain native discard, then repeated discard, partial rewrite, reopen and discard again.
7. Design multi-file creation and resize independently. Keep their capability results unsupported until durable publication and recovery tests pass.

For every stage, use a tiny two-extent deterministic fixture and compare all logical bytes after recovery. Test interruption after each coordinator, marker, CID, payload, mapping, flush and cleanup boundary, including torn patch bytes. Pending descriptor and direct extent reads must reject incomplete publication. Cover missing authorization, hard-link aliases, replacement/truncation of a later participant, foreign/symlinked/truncated markers, coordinator checksum errors, failed-writer poisoning and repeated recovery.

Add independent QEMU `twoGbMaxExtentSparse` check/convert comparisons for standalone and authorized backed images, including recovered states. Coordinate subprocess tests with the repository's process-boundary guard so inherited advisory-lock descriptors do not produce test races. Host and QEMU evidence does not establish VMware native acceptance or power-loss persistence.

## Historical design: allocation into existing grain tables

This increment is implemented. The following records its original TDD design
and the previously unsupported boundary. Its first allocation extension covers one
previously unmapped 64 KiB grain in a standalone extent with existing primary
and, when present, redundant grain tables. It does not introduce grain-table
or directory creation, parents, native discard, multi-file creation or resize.

The first failing test should reuse a tiny two-extent descriptor whose second
extent has an unmapped grain. Write a few bytes inside that grain, then require
zero contents around the write, an unchanged first extent, matching primary
and redundant GTEs, and an appended, fully initialized physical grain. Compare
every logical byte after reopening through both the reader and writer. The
current `Unsupported` result makes this test fail before enabling allocation.

Retain validated primary and optional redundant GTE addresses after the existing
parser establishes metadata ownership and redundant mapping agreement. Move
mutable mappings and ZERO masks under the backend state mutex, and increase the
shared cache reservation to cover the additional retained arrays. Missing grain
tables remain unsupported for allocation; do not create independently opened
component writers or component CID epochs.

For one grain, reuse a bounded extent record with a zero-initialized 64 KiB
payload containing the requested bytes. Append the payload with patch order 0,
update the redundant GTE with order 1 when present, and update the primary GTE
with order 2, or order 1 without redundancy. Bind the extended physical length
and original digest to that record. Publish new cached mappings only after a
successful complete-set commit; an interrupted writer remains poisoned until
reopened. A native ZERO grain also initializes from zeros before installing its
allocated mapping.

Before any mutation, project the entire write or zero call across all affected
extents. Check every required GTE location, grain-aligned physical tail,
representable sector entry and checked length increment. Simulate all proposed
appends per extent and enforce the aggregate 33 GiB physical bound. Account for
every resulting grain transaction, complete-set digest scan, old/proposed parser
pass, coordinator encoding and scratch/cache reservation against the remaining
cumulative budgets. A missing table, unsupported later extent or exhausted
budget must refuse the complete call before CID, file length, payload, mapping
or sidecar changes. Successful multi-grain execution may still complete a
documented prefix on subsequent I/O failure.

The existing coordinator gives CID a distinct durability barrier: complete-set
validation precedes durable coordinator publication; all participant markers
become durable before image mutation; the descriptor CID record is flushed
before extent patches. Payload, redundant GTE and primary GTE patch orders
express write submission order. The current replay flushes an extent after its
patches, so these orders do not establish individual persistence barriers.
Recovery relies on the durable complete-set redo and pending markers, followed
by the final participant flush before marker cleanup. Do not claim native
power-loss guarantees from submission order or host fault injection alone.

After the first single-grain test passes, add cross-boundary allocated-to-hole,
hole-to-allocated and two-hole cases; partial writes, zeroing, ZERO replacement
and clipped final logical grains. Exercise every coordinator, marker, CID,
length-growth, payload, redundant/primary mapping, flush and cleanup cut. Include
torn GTE bytes, missing authority and a substituted later participant, requiring
refusal before recovery mutates another file. Reopen successful recoveries and
compare exact bytes and ownership, then add independent QEMU conversion of
ordinary and recovered fixtures. Pending descriptor and direct extent readers
must continue to refuse incomplete transactions throughout this extension.

## Historical first append-based missing grain-table increment

The append-based profile is implemented and covered by the previously failing
`missing_table_creation_precedes_first_payload_in_empty_extent` regression.
The primary/redundant publication-cut test also covers torn GD and overhead
bytes; an independent QEMU conversion checks the resulting logical contents. The reader's ownership rules impose
an additional constraint beyond the existing-table allocation increment:
`src/vmdk.rs::parse_mode` requires each complete GT below the header's metadata
`overhead` boundary, and each mapped payload at or above that boundary. Appending
GTs at physical EOF and merely increasing overhead would engulf existing mapped
payloads. A valid old/proposed shadow transaction cannot bypass this constraint.
The ordinary single hosted allocator also requires existing GTs; its payload
append is not a ready-made missing-table implementation.

The implemented append-based profile requires an entirely unallocated target
extent, existing primary and optional redundant GDs, grain-aligned EOF, and at
most one absent GT pair per target extent for the complete request. Existing
mapped payloads, GD creation and general metadata relocation remain unsupported.
Unused historical payload bytes without mappings do not themselves constitute
active ownership, but must remain covered by original digest reconstruction.

The first failing test clears both GD entries in the second tiny extent,
then requests a write spanning the descriptor boundary. Require successful
allocation with unchanged first-extent neighbors and zero second-extent
neighbors, complete matching GTs, valid GD pointers and overhead, exact physical
growth, and exact reader/writer reopening. An initially empty two-grain extent
should then prove that two touched grains sharing one GT allocate that table
pair only once. Retain an unsupported case with mapped payloads in the target
extent and a later absent table; refusal must preserve every participant and CID.

For the bounded append profile, reserve a grain-aligned metadata arena after the
old EOF containing complete 2 KiB primary and, when required, redundant tables.
Initialize every unused and beyond-capacity GTE to zero. Place payload grains at
or above the proposed new overhead, initialize their full physical contents,
and patch the existing GD entries and overhead field in the same bounded extent
record. Include the descriptor CID in the complete-set coordinator when starting
an epoch. Publish retained table slots, mappings and masks only after commit.
Increasing overhead over unreferenced historical bytes must not expose those
bytes as logical data.

Whole-request planning must deduplicate tables by extent and GD index, project
metadata arenas and every payload append, and check sector encodings, aggregate
physical size, coordinator size, patch count and cumulative budgets before any
CID, sidecar or image mutation. There is an ordering constraint even when an
extent is initially entirely unallocated: an earlier requested grain may gain a
payload before a later missing table is created. Raising overhead at that later
point would engulf the newly allocated prefix. Either reserve and publish all
necessary table metadata for that extent before its first payload transaction,
or refuse the combination unchanged during complete-request preflight. The
smallest one-pair profile must take the latter route when another pair is needed;
successful multi-grain requests sharing the same planned pair may reuse it.

Newly present GTs add 512-entry scans per table, which are absent from the measured
validation cost of missing tables. Charge new table bytes, ownership/cache work
and scans for every complete-set old/proposed validation and replay pass. Update
the retained validation cost after creation, or use a proven worst-case bound
covering all possible tables. Existing fixed-geometry allocation cost estimates
alone are insufficient for table creation.

The implemented padding route preserves overhead: place full, sector-aligned tables in
previously unclaimed metadata padding below the existing boundary. The reader
claims the header, full descriptor reservation, GD ranges and complete existing
GTs; proposed tables must overlap none of those intervals or each other. Derive
candidate intervals from validated ownership, conservatively reserve complete GD
sectors, and bound the search and retained interval cache. Padding bytes need not
be zero merely because they are unclaimed: journal their original contents and
initialize complete replacement tables. Revalidate both shadow states and use
independent native fixtures before extending the advertised profile. This is a
separate bounded route, not permission to overwrite arbitrary metadata padding.

Fault cuts must cover coordinator and marker publication, CID persistence,
length growth, full table and payload initialization, overhead and both GD
patches, participant flush and marker cleanup. Add torn GD/overhead bytes,
substituted participants, missing authority and repeated recovery. Compare exact
logical bytes and ownership, then convert ordinary and recovered fixtures with
QEMU. Durable redo and pending markers provide recovery; table, payload and GD
patch submission order does not establish individual persistence barriers.

Current limits: one newly created table pair per target extent per call, with
that pair containing its first touched grain. Existing-table prefixes followed
by a missing table refuse before CID or sidecar publication. Creation reserves
64 KiB for table metadata and 64 KiB for the first initialized payload; later
grains sharing that table append only payload. A target extent with any mapped
payload remains unsupported for this append route. Padding-based placement and
metadata relocation remain future work. Cached table slots and validation scan
costs update only after the complete-set transaction succeeds.

## Historical first metadata-padding increment

An allocated extent cannot raise its metadata overhead over active payload.
For one missing GT pair per extent per call, the writer now first-fits a complete
2 KiB table or contiguous 4 KiB redundant pair into a sector-aligned unclaimed
range below the unchanged overhead. Protected intervals include the entire
header sector, the complete descriptor reservation even when blank, complete
outward-rounded primary/redundant GD sectors, and every full existing GT.
All intervals come from geometry validated by the normal ownership parser.
Bytes in the chosen hole need not be zero: their exact original contents are
journaled and the entire replacement tables initialized, including entries
beyond capacity. Scratch interval storage has a shared cache reservation before
allocation; retained fixed metadata is included in the opening reservation.

Complete-call preflight selects the hole, projects all payload appends and
checks remaining cumulative limits before CID or sidecar publication. A request
without sufficient padding refuses unchanged. Unlike append-based metadata,
padding tables cannot engulf newly allocated prefix payload, so an existing-table
prefix can precede the missing table. The complete-set extent record binds old
padding, new table bytes, GD pointers and initialized appended payload. Header
overhead never changes. Slot cache updates after commit make later calls protect
new tables while searching for another hole. GD creation, multiple new table
pairs in one extent/call and general metadata relocation remain unsupported.

The first red fixture grows declared capacity to 513 grains while retaining its
first mapped grain and first GT. Writing grain512 initially returned Unsupported;
it now selects sector26 in the original small extent and grows physical length
by one payload grain only. Tests preserve original mapped bytes and overhead,
recover mixed old/new bytes in nonzero holes at publication cuts for both primary
and redundant profiles, check fresh-hole selection after a second call, and
refuse no-space requests before modifying the descriptor or participants.

## Multiple missing tables in one request

The writer now owns one bounded request plan for the entire write or zero call.
Per-extent vectors identify all touched missing GD indices (at most 64 per 2 GiB
extent); a cache reservation stays alive through preparation and every payload
chunk. In allocated extents, planning reserves distinct sector-aligned metadata
holes before publication. A later pair that does not fit rejects the complete
call before CID, image bytes or sidecars change, even if the first pair fits.

An initially unallocated extent requiring several pairs, or an existing-table
prefix before its missing pair, uses one preparatory metadata-only complete-set
transaction. It appends a grain-aligned zero arena sized for all needed complete
primary/redundant tables, coalesces each GD span while preserving untouched
entries, and raises overhead to the arena end before any payload is allocated.
The largest possible arena is 256 KiB and each GD span at most 256 bytes, fitting
existing record limits. A single missing pair containing the first touched grain
retains its established combined arena/payload transaction. Full-call checks
project arena and payload growth, sector encodings, new 512-entry scans,
preparatory transactions, coordinator costs and cumulative work/cache budgets.

After a successful preparatory commit, mutable cached overhead and all installed
slots update together under the backend mutex. Later calls include those slots
in protected ownership and may reuse remaining arena padding. Zeroing prepares
once before processing chunks; it cannot repeatedly append the same arena.
Preparation failure poisons the writer. Recovery may complete metadata while
logical bytes remain at their old contents; a later payload failure may leave a
one-grain completed prefix. These are deliberate staged outcomes, not an atomic
whole-call promise. CID publication remains covered by the complete-set journal
and descriptor durability barrier.

Three established red tests now pass: allocated two-pair padding allocation,
empty two-pair arena creation and empty existing-table prefix allocation. A
second-gap refusal test proves full-call planning before mutation; a fixture
with no original metadata holes proves later reuse of the newly raised arena.
Phase-specific cuts cover metadata-only recovery separately from payload-prefix
recovery for primary and redundant profiles. Independent QEMU conversion compares
complete logical byte arrays for both new two-pair placement paths with redundant
tables. GD creation, general metadata relocation, backed COW, native split
discard/create/resize and VMware/power-loss acceptance remain separate work.
