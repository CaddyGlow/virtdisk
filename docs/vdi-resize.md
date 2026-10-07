# Native VDI capacity changes

`VdiWriter::resize(&mut self, size, ShrinkPolicy)` changes the native VDI 1.1
capacity and logical block count. This is an in-place operation with the retained
exclusive file lock, not export to a replacement image. The initial supported
profile is Linux, standalone dynamic VDI, 512-byte capacity alignment, at most
32 GiB virtual capacity, and allocation units at most 1 MiB. Fixed and backed
images are refused. Callers must own the image's dependency graph: external
children and readers cannot be discovered by a single-file writer.

Growth initializes added native map entries to FREE. An empty image can expand
its map arena and move its native data offset without relocating payload.
Allocated images grow within the existing arena or enlarge it through journaled
allocation-unit rotations. There is no total allocated-payload relocation limit
within the supported profile. The logical block map is capped at 1 MiB, and the
relocated data offset must fit its native unsigned 32-bit field. These bounds are
checked before changing the modification UUID.

Each rotation copies the first physical allocation unit to the container tail,
increments the native data offset by one allocation unit, and rotates every
private block index (zero becomes allocated-count minus one; other indices
subtract one). FREE and ZERO entries are preserved. Existing physical units
stay in place, retain unique dense ownership, and continue exposing the same
logical bytes. The payload append, full existing map and native data offset
change are one recoverable transaction. A rotation copies only one allocation
unit and one bounded map, rather than the entire allocated image. It retains
the original virtual capacity. Cached ownership changes only after commit.

Rotations repeat until there is space for the requested map. The final capacity
transaction then initializes additional entries and publishes the new size/count.
A completed relocation prefix remains valid at the old capacity after failure;
reopening replays any pending stage before continuing. The live writer is poisoned
on any failure after mutation begins, preventing reads or writes with stale cached
geometry. No file reopen, lock gap, replacement image, or unchecked overlapping
payload copy is involved. A partial hidden suffix is zeroed in a separate bounded
transaction before rotations, preserving zeroes throughout interrupted growth.

With a 1 MiB map and minimum 512-byte units, at most 2,048 rotation stages are
needed per growth operation. Each stage computes and checks the complete
container digest and validates old/proposed native views, so large images or many
stages may require substantial I/O. Progress callbacks, explicit cancellation,
and finer operation-work limits are future interface work; memory and stage
counts are bounded today.

Shrink Reject refuses all reductions. RequireZero scans the complete removed
logical tail before mutation; this does not establish filesystem safety.
AllowDataLoss explicitly permits removing nonzero bytes. Removed private units
are discarded individually through the archived-tail journal, swapping their last
physical owner when needed to preserve dense allocation ownership. The final
transaction changes capacity and count. A failure can leave a completed discard
prefix while retaining the old capacity; recovery restores the last interrupted
transaction and never promises whole-operation rollback.

Both growth and shrink zero the private partial boundary's entire hidden suffix,
so regrowth cannot disclose prior discarded bytes or unused block padding. These
zeroes, new map entries, native header changes and bounded arena relocation or
extension use sidecar transactions with full old/proposed reader validation. The
fresh native modification UUID is persisted before mutations. Transaction failures
poison the live writer until it is reopened for recovery. Same-size calls are
no-ops; range, policy, profile, map-space and ownership errors occur before UUID
mutation. The existing transaction engine caps records at 4 MiB. Each resize
stage moves or zeroes at most one 1 MiB allocation unit and rewrites at most a
1 MiB map; shrink discard retains its separately documented tail-archive limits.

Tests cover retained data, discarded-tail regrowth, adversarial nonzero hidden
padding, empty/allocated map expansion, out-of-order physical owners, refusal
without mutation, and interruption at each final transaction stage. An opt-in oracle asks native VirtualBox to inspect and
flatten the resized image and independently compares QEMU's flattened bytes.
Native Windows crash/power-loss behavior and cancellable operation budgets remain
separate validation/implementation work.

Relocation interruption tests exercise each final publication stage for maximum-
capacity growth and every journal boundary at the first, middle and last rotations
of a 31-stage relocation.
They verify preserved old capacity after an interrupted prefix, complete hidden-
tail zeroing, subsequent growth, and a new final-block allocation after recovery.
The native VirtualBox/QEMU oracle includes three preallocated physical owners
through relocation before shrink and regrowth. These deterministic
fault cuts establish hosted recovery behavior; they do not establish power-loss
semantics of every filesystem or operating system.
