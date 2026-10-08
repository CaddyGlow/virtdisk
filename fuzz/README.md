# Independent virtdisk fuzzing

Owned targets: qcow2, vdi, vmdk, vhdx, raw-write, qcow2-write, vdi-write, vmdk-write, vhdx-write, vhdx-chain. Reader harnesses have input/read/output budgets; raw-write checks bounded write/zero/resize/reopen sequences against an independent byte model in harness-owned temporary files. Harnesses do not depend on another project’s fuzz package. Malformed input is expected; panics and oracle mismatches are findings. Seeds include structural inputs and regression fixtures where available. Production defaults remain unchanged.

Run `cargo test --manifest-path fuzz/Cargo.toml --locked` and `cargo clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings`. Install honggfuzz 0.5.62 plus GCC/binutils/libunwind/liblzma development libraries, then run `python3 scripts/fuzz-campaign.py --iterations 10000`. Replay with `cargo run --manifest-path fuzz/Cargo.toml --locked --bin replay -- TARGET FILE`.

The deterministic corpus smoke test uses up to four workers, capped by available
parallelism. Every seed, truncation, and mutation is still replayed; independent
temporary files permit concurrent durability work. Failures identify the target,
seed index, and input variant.

Native writer tests construct each immutable parent profile once and retain its
bytes. Each replay writes and syncs its own parent copy before opening the child;
all operation, reopen, and parent-immutability assertions still run. Instrumented
campaigns and standalone replay binaries continue constructing their parents for
every input.

The weekly/manual workflow instruments code and retains seed/production-source/harness hashes, tool versions, raw logs, summary counts, and findings even on failure. Source changes during a campaign invalidate its result. Each case has a five-second timeout. Bounded smoke campaigns do not establish absence of bugs or replace sustained sanitizer campaigns. Fuzzing operates only on memory or temporary files created by the harness; no input-supplied host paths are opened.

The `qcow2-write` target uses at most 385 input bytes, 64 operations and 192 KiB
virtual capacity. Append-only COW and snapshot metadata can make physical files
larger than logical capacity. It exercises
private images and authorized raw-backed overlays against an independent byte
model, including cross-cluster writes, zeroing, invalid bounds, unauthorized-open
refusal, flush/reopen, exact reference-count validation and base immutability.
Filenames are fixed inside a harness-owned temporary directory. Allocation
interruption coverage currently lives in deterministic library unit tests; no
production failure-injection API is exposed to the fuzz target. Linux is required
for journaled overlay allocations; private payload replay works elsewhere.

The three native writer targets use at most 193 bytes, 32 operations, and 2 MiB
virtual disks. Both sparse standalone and native parent/child cases compare every
write, partial zero, flush, reopen, and invalid range against an independent byte
model. Parents and filenames are created entirely by the harness; final parent
bytes must remain unchanged. These journaled targets currently run on Linux.
Raw writer operations include explicit generic discard with zero fallback; QCOW2
operations include aligned native discard and partial-range fallback, checking
that discarded ranges remain zero across reopen and suppress backing bytes.
Failure interruption remains covered by library unit tests.

The `vhdx-chain` reader target accepts at most 8 MiB of child-container bytes.
It creates a fixed 6 MiB native parent in a private temporary directory and
explicitly authorizes only that parent. Embedded names never authorize other
files. Valid seeds contain a partially present payload and sector bitmap;
additional seeds cover explicit-zero masking, overlapping bitmap ownership,
and a missing bitmap. Reads cross sector/block boundaries and extent visitation
stops after 32 callbacks. Metadata, chain depth and work budgets are tightened.

QCOW2 writer mutations also include native capacity transactions, keeping capacity
at most 192 KiB and updating the byte model for zero growth and explicit shrink
policies. Authorized backed resize preserves saved states and masks inherited
bytes in the newly grown suffix after shrink/regrowth.
Raw preallocation operations preserve the logical model and check ranges; hosts
without preallocation support must report Unsupported. Native VDI cases use
sparse standalone or differencing profiles and include whole-block discard plus
a sector-clipped final block. Replay checks zero masking and unchanged parent
bytes after deallocation and reopen.

Native VDI, VMDK, and VHDX stateful cases also change capacity within 512 bytes..2 MiB using
explicit shrink policies; rejected requests compare exact container bytes, and
backed resize remains unsupported (VDI and VHDX allow an unchanged-capacity no-op). Growth
and shrink/regrowth update the byte model. VDI, hosted sparse VMDK, and VHDX cases require native
Deallocated results for full units and final clipped units; partial fallback
zeroing remains explicit. Seeds repeat discard, partial rewrite, reopen and
discard to verify that newly allocated mappings can be released again.
Parents remain immutable after all operations.

QCOW2 reader seeds include a valid native disk snapshot sharing L2 and payload
owners. The reader target lists directories, audits ownership, opens up to two
saved states, and reads bounded selected ranges under tightened work, cache,
metadata and decompression budgets. Snapshot directory truncations and mutated
refcounts exercise expected parser errors.

The QCOW2 writer model also creates, reverts, and deletes up to four standalone or authorized backed native disk snapshots.
Each retains an independent saved capacity/byte model checked through audited
views after reopen and at the final checkpoint. Duplicate/invalid requests and
unauthorized-open refusal preserve exact container bytes. Active write, zero,
discard, resize, and reopen must preserve every saved state. Deterministic seeds cover snapshot creation,
saved-state selection, deletion, missing IDs, ID reuse, and subsequent active
mutations. Both standalone and authorized backed lifecycle sequences are replayed.

VHDX chain seeds include the ordinary drive-rooted parent hints emitted by the
Microsoft provider, alongside rejected alternate-stream and device-path hints.
All resolution remains confined to harness-owned explicitly authorized parents.
Real compressed Windows-produced 512/4096-sector partial-child fixtures are
covered by `tests/vhdx_windows_native.rs`; those fixtures are not fuzz input paths.


The VMDK writer target appends modes 4/5/6/7 for Linux standalone split sparse
allocated-grain writes, existing-table hole allocation, missing-table creation
and unaligned missing-table refusal. Two 512-byte logical extents keep an independent
1024-byte model. At most eight records exercise cross-extent writes, zero
fallback, reads and reopen. The unaligned missing-table profile verifies complete-file immutability for
unsupported allocation; all profiles check native discard/resize and missing authority.
Existing native mode seed order is preserved.

Temporary storage affects the five-second case limit because writer operations
include durable flushes. This checkpoint uses command-scoped
`TMPDIR=/data/cache/virtdisk-fuzz-model-tmp` after observed journal stalls on the
host's default temporary filesystem. Keep the case timeout unchanged and record
campaign outcomes independently; uninstrumented replay timing alone does not
establish instrumented timing.
