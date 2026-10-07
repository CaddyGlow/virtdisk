# Native VHDX sparse allocation transactions

## Required behavior

`VhdxWriter` opens a clean standalone fixed or dynamic image under an exclusive
file lock. Opening never changes its bytes. A write into a zero/not-present block
allocates a new complete payload block, initializes untouched bytes to the
reader's existing zero semantics, and publishes the corresponding fully-present
BAT entry through the native VHDX redo log. Authorized inherited or partially
present differencing blocks use native sector-bitmap transactions described in
[vhdx-partial-write.md](vhdx-partial-write.md). Explicit ZERO replacement retains
whole-block initialization to avoid revealing parent data.

A write may partially complete across multiple logical blocks. Each newly
allocated block is one independently recoverable metadata transaction. Existing
fully-present payload writes retain the documented partial-write behavior.

## Persistent ordering

1. Validate the image and locked-file identity; retain the bounded BAT map.
2. Before any file mutation in this writer epoch, durably install fresh file and
   data UUIDs using the inactive header, sync, the other header, and sync.
3. Check header sequence headroom and reserve transaction memory/work before
   allocation. Extend to an aligned fresh payload extent; untouched bytes are
   zero. Write the requested payload slice, then sync payload and file length.
4. Construct an 8 KiB native log entry: one 4 KiB descriptor sector and one
   4 KiB data sector carrying the entire affected BAT sector. Its descriptor
   preserves the removed leading/trailing bytes. GUID, sequence, tail, CRC32C,
   FlushedFileOffset and LastFileOffset must match the actual transaction.
5. On this writer epoch's first allocation, append a fresh 1 MiB log region,
   write the valid entry there and sync it **before** activating that new region
   through redundant headers. Until activation these bytes are unreachable;
   the old clean headers remain usable. This avoids an activation window with a
   nonzero log GUID and no valid matching entry.
6. Reuse this log region for later allocations in the same writer epoch. First
   reactivate the same epoch's GUID through redundant headers, with the previous
   committed entry still valid. Write the next entry into the alternate 16 KiB
   slot, preserving the previous complete entry, then sync it. Each entry has a
   self tail; the newest valid entry alone is the active sequence.
7. Write the complete BAT sector to its final location and sync it. Update the
   in-memory logical map after successful publication persistence.
8. Clear LogGuid through the inactive header and sync, then the other header and
   sync. Keep the committed log entry unchanged for the next transaction.

Any metadata/header/log I/O failure poisons further mutations on the handle.
The caller closes it and runs `recover_vhdx`; retained native redo either restores
its prior complete BAT sector or publishes the newer complete sector. Payloads
which were persisted but never published remain unreachable allocated file space.
No rollback, payload-write atomicity, or automatic space reclamation is promised.

## Log-region lifecycle

A reopened writer must use a fresh log GUID before overwriting log space. The
initial implementation appends one fresh log region per writing epoch that
allocates payload blocks. Existing log space is left intact and becomes
unreferenced when the new region is activated. This costs 1 MiB per such epoch;
compaction/log-space reclamation is future work. Within one epoch the two retained
entry slots avoid that overhead for each block allocation and tolerate torn entry
writes without destroying the prior valid record.

## Bounds and fault evidence

The existing parser budgets retain the BAT map and bound transaction scratch and
work. Geometry, BAT-sector offsets, appended extents, all additions, log entry
sizes and header/log sequence increments are checked before publication. Logical
writes remain bounded to the advertised capacity. Zero-only writes into holes
need no payload allocation. Native zero/discard metadata transitions and resizing
remain later operations.

Tests precede implementation and must cover:

- Opening sparse images without mutation; partial-block and boundary-crossing
  writes; reopening the resulting logical content; zero-only writes into holes.
- Repeated allocations in the same BAT sector and across BAT-sector boundaries.
- Independent QEMU check/convert for native output and written images.
- Cuts at payload sync, staged initial log sync, header activation, alternate log
  publication, BAT publication, and both log-clearing header syncs.
- Torn new log entries and partially written BAT sectors, with explicit native
  recovery preserving old or new complete logical mappings.
- Bounds, resource ceilings, sequence overflow, alias locking, poisoned handles,
  fresh identity epochs, and concurrent disjoint writes.

Process interruption and synthetic torn writes provide ordering evidence. Native
Windows crash fixtures and Windows runtime tests are separate release gates.

## Primary references

- [MS-VHDX headers](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/39d641c2-093c-4d4a-8c9d-bd4b9fc2ff31)
- [Native log](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/98acf5fe-7eff-43cf-b6c1-89cf00f24ade)
- [Log entries](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/19dc2735-9613-4b7f-a411-40243de2346e)
- [Log entry header](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/99aae9a5-2c0a-4ded-8169-54d541f785bd)
- [Data descriptors](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/6bd25957-a56b-45e6-996f-ab48091cf080)
- [Replay](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/0d588e33-23a6-4c71-b27f-87d97ac3e914)

## Native discard

`VhdxWriter::discard(offset, length, policy)` supports Linux dynamic/differencing
profiles with the reader's validated geometry. LeaveBlocksAllocated/fixed images
require explicit zero fallback; strict native requests fail before UUID mutation.
Offsets align to payload blocks; lengths cover whole blocks or the clipped final
logical block. One existing native BAT-sector redo transaction per block writes
ZERO with no payload offset. Released payload bytes remain physically present;
sector bitmap allocations are retained. This releases container mapping ownership,
not host holes or physical EOF. A fresh 1 MiB native log may grow the file.

ZERO masks backing content, including an entire partially-present bitmap block.
No bitmap bits are changed. Retained immutable reader state is bypassed with a
bounded per-session zero mask so subsequent partial writes initialize zeroes.
First/reused log cuts, authorized parent models, full/final units and independent
standalone QEMU check/convert exercise this contract. Native child QEMU/Windows
interoperability and actual Windows crash recovery remain separate release gates;
Windows native discard stays Unsupported unless explicit zero fallback is allowed.
