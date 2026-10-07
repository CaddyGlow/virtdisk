# Native VHDX capacity transactions

Implemented initial profile: exclusively locked Linux standalone dynamic VHDX,
positive capacity aligned to its validated logical sector size, at most the native
64 TiB geometry limit, and growth fitting the existing BAT region. Fixed images,
authorized parent chains, and BAT-region relocation remain explicit unsupported
profiles. Existing parser/cache/work limits also apply. This changes native
metadata in place rather than publishing a flattened replacement image.

The [Virtual Disk Size item](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/bb51c7bc-63be-4be6-a77c-f1684573033c)
is an eight-byte size aligned to LogicalSectorSize. The
[BAT layout](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/af7334e6-ad2c-4378-9b81-afc1334a6ee7)
interleaves sector-bitmap slots between chunks even for standalone dynamic images;
required entries must fit the native BAT region. The
[native log rules](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/98acf5fe-7eff-43cf-b6c1-89cf00f24ade)
require redo logging for metadata and prohibit payload updates through the log.

Design:

1. Validate range, policy, profile, BAT capacity, metadata-item location, cache
   reservation, header/log sequence range and declared stage-work limits before
   UUID/header mutation. RequireZero
   scans every removed logical byte first. Same-size calls are no-ops.
2. Persist the existing native file/data GUID epoch through both redundant
   headers before any payload or metadata mutation.
3. Persist zeroes throughout the private final partial block's hidden suffix
   before publishing changed capacity. These are ordinary payload writes rather
   than log descriptors. Interruption may leave a completed zeroing prefix at the
   original capacity; whole-operation rollback is not promised.
4. Growth initializes all additional logical payload BAT slots to native ZERO
   using bounded sector redo transactions, preserving existing entries and zero
   bitmap slots. Shrink releases removed private mappings through the existing
   logged native ZERO operation. These stages retain original virtual capacity.
5. Publish the sector containing VirtualDiskSize through the same native redo
   protocol: fresh/reused matching LogGuid and entry sequence, durable log before
   metadata publication, durable target sector before clearing LogGuid through
   both redundant headers. Keep the retained previous record until the alternate
   record is durable; fresh writing epochs use a fresh log region.
6. Validate complete old/proposed native views for each initialization/size sector
   using the retained cumulative parser budgets and locked source. Validation may
   exhaust a cumulative budget after a completed prefix; budgets are never reset.
   Update cached length/map ownership only after successful publication. Any
   mutation failure poisons the live writer until explicit native recovery and
   reopening. A completed prefix remains structurally valid at old capacity.

Native payload/data offsets and metadata-item placements come from the validated
reader under the retained lock. Recovery must replay the complete active native
entry before reading changed metadata; no host sidecar is introduced. Deterministic
fault cuts cover initialization, size-sector redo, fresh/reused log activation,
publication and redundant-header clearing. QEMU independently checks and converts
clean and interrupted images. These checks do not establish Windows power-loss
correctness, guest filesystem safety, fixed/backed resizing, or BAT relocation.

Verification starts with behavioral tests failing because the native resize method
was absent, followed by integration tests for grow/shrink/regrowth, adversarial
hidden padding, 4096-byte logical sectors, moved size-item placement, chunk-bitmap
interleaving, immutable invalid-request refusal, and fixed/backed refusal. The
initial size-item profile must fit within one 4 KiB redo sector; spanning items
are refused before UUID mutation. RequireZero scans logical bytes, rather than
asserting guest filesystem or partition safety.

Fault tests cover first/reused log epochs at every initialization and final size
publication boundary, persisted payload zeroing prefixes, and a torn size item
restored from its complete retained redo. Immutable recovery preserves the source;
writable recovery is idempotent and subsequent resize succeeds. An independent
QEMU oracle replays fresh and reused size-sector logs for growth and shrink,
asserts the flattened file length equals the expected native capacity, and compares
all payload bytes. A clean QEMU oracle checks exact shrink/regrowth bytes.

Capacity shrink releases native mappings without promising host-hole punching or
container truncation. The retained native log and unreferenced payload allocations
await separate compaction. The fresh/reused transaction protocol is shared with
sparse allocation and discard. Existing operation budgets remain cumulative,
including parse validation, metadata, work and live-cache reservations. Explicit
progress/cancellation, BAT-region relocation, fixed/backed resize and native
Windows/power-loss validation remain future work.
