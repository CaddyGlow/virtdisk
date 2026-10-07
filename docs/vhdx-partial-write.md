# Native VHDX partial-sector writes

Authorized differencing writers now publish native PARTIALLY_PRESENT payload
entries and sector-bitmap updates. Standalone and fixed images retain their
existing permitted states: PARTIALLY_PRESENT remains forbidden without a parent.
Public `write_all_at` and `write_zeroes` accept arbitrary byte offsets, retaining
512-byte and 4096-byte logical sector geometry. Parent paths, linkage UUIDs,
opened identities, depth and shared parser budgets remain validated at open.

The [payload state rules](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/01da203b-b3d7-487d-928b-22a460bbe177)
require a valid allocated sector bitmap before PARTIALLY_PRESENT is visible.
[Bitmap bits](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/43b647c6-6e6c-48c3-a436-3deebd622f44)
select private child sectors or inherited parent sectors, with the low bit first.
[Native log entries](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/19dc2735-9613-4b7f-a411-40243de2346e)
carry exactly one data sector per data descriptor; zero descriptors carry no data
sector. Payload data is persisted outside the log.

## Transaction ordering

1. Retain mutable payload states and bitmap offsets alongside the writer's BAT
   map, reserving their storage under the existing shared budget. Resolve
   inherited sectors directly through the retained authorized parent rather than
   the child's original cached immutable mapping.
2. If the chunk has no bitmap, append a fresh aligned 1 MiB extent. Log a zero
   descriptor for the complete bitmap and a data descriptor for the bitmap BAT
   sector. Persist zeroes before publishing SB_BLOCK_PRESENT, then clear the log
   through both redundant headers. All payload entries still retain their original
   states. An interrupted completed bitmap allocation changes no logical bytes.
3. Allocate a private payload extent when an inherited block first becomes
   partial. For every touched logical sector, preserve unmodified edge bytes
   through read/modify/write of the current resolved sector. Full-sector writes
   need no parent read. Persist the changed sector payload before bitmap updates.
4. Log the modified 4 KiB bitmap page and, for a new private payload, its BAT
   sector with PARTIALLY_PRESENT. Persist all metadata targets before clearing
   the log. Existing partial blocks normally need only a bitmap-page descriptor.
   Cached state changes only after successful commit.

Operations split at payload-block and bitmap-page boundaries. One bitmap page
covers 32,768 logical sectors; the implementation supports the existing native
1--256 MiB payload block sizes without buffering a whole payload block. Scratch
holds a sector buffer, bounded metadata pages and the encoded log entry. A fresh
bitmap's zero descriptor represents 1 MiB without storing that much redo data.
Cumulative metadata, work, live cache and recovery limits are never reset.

The shared redo encoder supports bounded distinct targets, zero descriptors and
multiple data descriptors with checked offsets, overlap rejection, sequence
copies, CRC32C, descriptor count and exact entry length. Bitmap initialization
uses an 8 KiB entry; initial payload publication uses a 12 KiB entry. Existing
single-target allocation, discard and resize entries remain 8 KiB. Two disjoint
16 KiB slots retain the previous complete record until the alternate record is
durable. Each current entry names itself as its tail. A new writing epoch still
appends a fresh 1 MiB log region and changes header GUIDs before payload mutation.

## Data and failure contract

Untouched bitmap bits and other payload owners in the same chunk remain unchanged.
ZERO/Undefined/Unmapped blocks keep their zero-read semantics: writing to a ZERO
block uses fully-present zero-initialized allocation, because unset partial bits
would incorrectly reveal parent bytes. Whole-block discard still logs native ZERO;
bitmap storage is retained. Explicit partial-discard fallback writes private
zero sectors and does not claim host deallocation.

Any metadata, log, header or partial-sector operation failure poisons the live
writer until explicit recovery and reopen. Persisted writes to previously private
sectors may partially complete like ordinary payload writes. Newly private sectors
remain inherited until bitmap publication; completed metadata stages can remain
as a valid prefix. Full-operation rollback and payload-write atomicity are not
promised. Immutable recovery changes no source bytes. Writable recovery requires
explicit parent authorization and replays complete metadata before exposing reads.

## Evidence and remaining gates

TDD began with a native-state assertion failing because previous writes promoted
whole blocks to FULLY_PRESENT. Tests now cover native partial entries, unaligned
512/4096-sector edges, live/reopened reads, shared bitmap owners, separate chunks,
final clipped sectors, zero-mask replacement, immutable parent bytes, and native
metadata interruption stages. Torn multi-descriptor log sectors fall back to the
retained complete prior record; torn bitmap or BAT publication replays the newer
complete record.

The independent QEMU protocol oracle replays fresh/reused zero-plus-multiple-data
records on standalone fixtures, checking exact native capacity and all flattened
bytes. This establishes encoder/replay interoperability for that native log
profile. Installed QEMU's differencing support remains unavailable; standalone
flattened export is a separate logical-content oracle, not native child acceptance.
Actual Windows-produced partial children, Windows opening of emitted children and
power-loss recovery remain external gates. Bitmap/log reclamation, optional
fully-present promotion after every bit becomes private, explicit progress and
cancellation, mixed-capacity parent chains and advanced parent resize remain
separate work toward the complete plan.
