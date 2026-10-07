# Implementation status

This is a progress ledger for [the implementation plan](implementation-plan.md).
The full plan remains active. An implemented initial profile does not complete
the format family or its release gates.

## Implemented foundations

- Separate `ReadAt` and `WriteAt` contracts; existing immutable reader API kept.
- Binary format detection with explicit raw selection and no corrupt-container
  fallback.
- Concrete profile/geometry/validation/capability inspection via `InspectImage`.
- Bounded cancellable copying, logical comparison and SHA-256 hashing.
- New-output conversion with logical verification and no-overwrite publication.
- New-output grow/shrink with explicit tail policy and zero-filled growth.
- Streaming extent visitation, callback cancellation, view clipping and budgets
  for QCOW2, VDI, VMDK and standalone VHDX. Raw conservatively reports unknown
  allocation.
- Caller-owned dependency graph for QCOW2/VDI/VMDK/VHDX external snapshots: authorized ancestral
  edges, alias/cycle/replacement checks, branch-aware leaf deletion, flatten,
  merge to a new output and content-preserving QCOW2 rebase to a new overlay.
- Cancellable new-output compaction across supported export profiles; zero-omitting QCOW2
  export and sparse raw copying, with logical verification before publication.
- Common ImageWriter factory retains native writer locks and authorized parent access.
- Optional CLI for info, map, hash, compare, convert, compact, new-output resize,
  zero, explicit-policy trim, read-only check and QCOW2 snapshot create/list/export/delete/revert;
  applicable reader operations accept authorized parents.

## Current native profiles

| Format | Implemented | Remaining core work |
| --- | --- | --- |
| Raw | Regular-file reads; locked creation/writes/zero/flush/resize; Linux host discard/preallocation | Failure-injected storage backend and Windows native allocation/discard |
| QCOW2 | Existing bounded v2/v3 reader; standard v3 export; positional writer; Linux sparse allocation/shared payload and L2 COW; native whole-cluster discard and bounded capacity changes; redo journal/recovery; disk-only internal snapshot listing, ownership validation, saved views and bounded native creation/deletion/revert; authorized writable backing chains and external overlays | Refcount-table relocation, Windows journal persistence, broader native resize profiles, active compressed writes, backing-aware creation and advanced metadata |
| VDI | v1.1 fixed/dynamic reader/export; fixed/dynamic writer with durable modification UUID epochs; Linux transactional sparse allocation and authorized differencing COW; native VirtualBox file acceptance; journaled dynamic discard, tail reclamation and bounded standalone native resize | Fixed/backed resize, native compaction, broader discard profiles and Windows persistence |
| VMDK | Hosted sparse v1 and redundant tables; authorized flat/split descriptors; native hosted export; hosted payload writer; Linux journaled sparse allocation with redundant mapping recovery and authorized parent COW; standalone authorized monolithicFlat and twoGbMaxExtentFlat writes; bounded standalone hosted sparse resize; native whole-grain ZERO discard with parent masking | Parented flat and writable split sparse profiles, stream-optimized profiles, VMware-native acceptance |
| VHDX | Clean standalone fixed/dynamic reader; native dynamic export; immutable log replay; explicit locked native writable log recovery; payload writer with durable header GUID epochs; native logged sparse BAT allocation; authorized differencing, sector-bitmap inheritance and native partial-sector writes, native dynamic discard and bounded standalone dynamic resize | Broader resize profiles and host reclamation, Hyper-V/Windows-native runtime acceptance |

QCOW2 transactions are bounded, append-only and Linux-only. Multiply linked
images cannot be opened for journal-backed mutation. Pending journals require
recovery; path-backed read-only QCOW2 opening rejects them. Ordinary private
payload writes remain non-atomic. The precise recovery contract is documented
in [qcow2-write-recovery.md](qcow2-write-recovery.md).

## Test-driven evidence

New APIs began with failing tests. Behavior regressions also drove changes:
QEMU redundant VMDK tables and padded descriptors, conversion verification,
copy cancellation, capability reporting, and the journal hard-link alias bypass.

The repository now contains independent QEMU reader, export, writer, overlay,
and VHDX log-recovery oracles. Fault tests cover QCOW2/VDI/VMDK journal/allocation
persistence boundaries, redundant VMDK mapping order, and VHDX native logged
allocation/replay/header persistence boundaries.
These are host process/I/O simulations, not real power-loss evidence.

Key evidence sources:

- `tests/image.rs`, `tests/info.rs`, `tests/cli.rs`.
- `tests/raw_write.rs` and per-format reader/export/writer tests.
- `tests/qcow2_overlay.rs`, `tests/qcow2_pending.rs`, `tests/qcow2_overlay_writer.rs`.
- `tests/graph.rs`, `tests/graph_review.rs`, `tests/compact.rs`.
- `tests/vdi_writer.rs` and shared journal/VDI allocation fault tests.
- `tests/vhdx_allocate.rs` and native allocation persistence fault tests.
- `tests/vhdx_log.rs`, `tests/vhdx_recover.rs`, `tests/recovery_review.rs`.
- Unit fault tests in the QCOW2 journal/allocator and VHDX native recovery.

Honggfuzz targets cover QCOW2, VDI, VMDK, VHDX readers, raw write/zero/resize/
reopen sequences, and model-based QCOW2 standalone/authorized-overlay writes. Deterministic replay shares the same harness functions.
Campaign receipts now record production code, manifests/locks, tool versions,
seed hashes and harness code, and reject source changes during a run.

VDI/VMDK/VHDX writer stateful model/replay targets have now been added.
Measured smoke campaigns and snapshot sequence models are recorded in the
checkpoint sections below. Still required: parent/extent resolver fuzzing,
coverage-guided log-recovery sequences and sustained campaigns.
A bounded VHDX differencing/sector-bitmap malformed-reader target also passes
deterministic structural seed and crossing-sector byte-model tests.

## Milestone audit

| Plan milestone | Status |
| --- | --- |
| 1. API and inspection | Substantial implementation; opening/recovery policies, richer operation errors and operation limits still need consolidation |
| 2. Raw and generic operations | Core implementation; storage fault coverage, allocation policies and Windows runtime gates remain |
| 3. QCOW2 writes and overlays | Linux standalone/overlay sparse writes and recovery implemented; remaining native management operations incomplete |
| 4. VDI | Reader/export, transactional differencing COW and native VirtualBox acceptance implemented; native management incomplete |
| 5. VHDX | Reader/log replay/export, logged allocation and differencing COW implemented; native management and Windows acceptance incomplete |
| 6. VMDK | Hosted/descriptor chain readers and transactional hosted parent COW implemented; split sparse writes and stream profiles incomplete |
| 7. Advanced management | Incomplete: broader native snapshot profiles, persistent graph manifests, in-place rebase/commit, additional native discard profiles, constrained repair, incremental tracking and benchmarks |

Windows x64 cross-compilation and target Clippy pass; this is compilation evidence
only. Nix inputs are pinned by the tracked `flake.lock`.

No milestone release gate should be marked complete from this ledger alone.
Required host checks use the default Nix shell and fuzz checks use `.#fuzz`.
Independent QEMU tests must be explicitly run with `--include-ignored`.
Native Windows/Hyper-V and VMware runtime evidence is still missing.
Native VirtualBox 7.2.18r175117 file acceptance now passes two isolated-registry
oracles in tests/vdi_virtualbox.rs; this does not establish VM boot correctness.
Cross-compilation and host tests cannot substitute for those gates.

## Next implementation priorities

1. Shared journal platform persistence and VMDK writable split sparse/parented-flat profiles.
2. VHDX bitmap/log reclamation and advanced partial-block profiles; per-format native capacity and
   discard operations, plus QCOW2 refcount relocation.
3. Container write/recovery state-machine honggfuzz targets with fault injection.
4. Native QCOW2 backing-aware snapshot creation and broader compressed-state transaction support; broaden
   snapshot mutation profiles without weakening complete ownership validation.
5. Additional discard and native capacity operations; cancellation/progress and
   operation budgets across management paths.
6. Native hypervisor/Windows runtime gates, supported-platform persistence,
   performance and release documentation.

## Verified checkpoint: 2026-10-07

- Required host formatting and all-target/all-feature Clippy with warnings denied:
  pass in the default Nix shell.
- `cargo test --all-features --locked -- --include-ignored`: 194 tests passed,
  zero failures and zero ignored tests. Includes independent QEMU acceptance,
  readback and recovery checks; log retained at
  `/data/cache/virtdisk-host-tests-20261007.log`.
- Fuzz workspace formatting, all-target Clippy and locked tests: pass in `.#fuzz`.
- Windows x64 `cargo xwin check --all-features --locked` and target Clippy with
  warnings denied: pass. No Windows runtime claim follows from compilation.
- Six-target honggfuzz smoke: 101 iterations per target, zero crashes and zero
  timeouts. Receipt: `target/fuzz-runs/20261007T112350Z-2/summary.json`.
  Production/fuzz sources, manifests, Cargo locks, flake and `flake.lock` hashes
  matched that checkpoint; source-change detection remained false throughout.
  Subsequent changes require a new campaign before this evidence can describe
  the current production and harness sources.

This is bounded smoke evidence, not completion of sustained fuzz campaigns or
native hypervisor release gates. All seven milestone completion audits above
remain open.

## New behavior awaiting aggregate checkpoint verification

- Native VDI/VMDK/VHDX authorized differencing and graph integration pass focused
  tests. VHDX staged locators are validated against the final publication directory.
- QCOW2 shared L2 COW and native cluster discard pass focused fault/recovery and
  independent QEMU checks. Raw host hole punching passes allocation measurements.
- CLI zero/trim, authorization rejection, parent masking, and invalid-request
  no-mutation behavior plus authorized chain info/map/hash pass seven CLI tests; generic discard passes five tests.
- Native VirtualBox acceptance exposed a missing extended VDI sector-size field
  at header offset 468. All three creation paths now set it to 512; both native
  acceptance tests pass, including COW child flattening and exact logical bytes.
- The previous full-suite and six-target campaign receipts above describe the
  older checkpoint. New aggregate checks and expanded campaign receipts must be
  recorded separately before claiming the current tree verified.

- Native VDI validation now rejects nil creation and modification UUIDs, matching
  VirtualBox's supported header contract. Failing acceptance regressions preceded
  the parser fix; native fixed/dynamic/differencing file acceptance remains green.

## Aggregate native checkpoint: 2026-10-07, expanded profiles

- Full parallel `cargo test --all-features --locked -- --include-ignored` passes:
  260 tests, zero failures, zero ignored tests. Includes QEMU and both native
  VirtualBox file acceptance oracles. Log:
  `/data/cache/virtdisk-current-native-parallel-tests.log`.
- The same aggregate suite also passes with one test thread; this separately
  confirms logical behavior from subprocess concurrency.
- Required ordinary parallel tests, formatting and Linux all-target/all-feature
  Clippy pass. Windows x64 all-target/all-feature target Clippy passes.
- Parallel oracle testing exposed fork-time inherited file-lock interference.
  A shared test-only synchronization guard excludes subprocess spawning while
  writer unit tests hold locks, preserving parallel execution of those tests.
  Production nonblocking locks and recovery behavior remain unchanged.
- Ten-target honggfuzz deterministic replay/corpus tests (seven tests), target
  builds, formatting and Clippy pass. Ten-target measured smoke passes with
  101 iterations per target, zero crashes/timeouts, and source-change detection
  false. Recorded hashes matched that checkpoint; subsequent native-management work
  requires a new matching campaign receipt.
  Receipt: `target/fuzz-runs/20261007T120257Z-2/summary.json`.

Native Windows/Hyper-V and VMware runtime acceptance, sustained campaigns,
remaining native management and advanced profiles still prevent full plan
completion. No milestone release gate is declared complete.

## Native management additions undergoing aggregate verification

- Missing public API and CLI tests preceded common `ImageWriter::create` and
  `create` CLI implementation across all five families. Tests establish no
  overwrite, retained locks, initial zero reads, bounded writes and native reopen.
- Linux raw `preallocate` reserves host allocation without changing bytes or
  capacity; tests measure allocated blocks, bounds and locks. Generic container
  preallocation is explicitly unavailable. CLI supports raw preallocation.
- Native QCOW2 growth/shrink uses the retained writer lock and redo journal,
  grows L1 coverage, releases removed mappings/refcounts, and prevents stale
  bytes after regrowth. Shared/private shrink and empty growth interruption tests
  and independent QEMU check/flatten tests pass. See `qcow2-resize.md` for bounds.
- `ImageWriter::resize` and `resize-native` expose native raw/QCOW2 resize with
  explicit tail policy. Unsupported families reject without changing the file.
- Dynamic VDI discard journals ZERO mapping, relocates the final owned payload
  when removing an interior block, and archives/truncates the physical tail. The
  common writer and CLI test confirms real tail reclamation and parent masking.
- Authorized standalone monolithicFlat VMDK descriptor/extent writes retain both
  locks and sync a fresh CID before payload changes. Independent QEMU tests pass.
  The tests also exposed and fixed uppercase-short-CID update corruption.

The previous 260-test and ten-target smoke receipts remain historical evidence.
New aggregate results and updated stateful-model campaigns must be recorded for
these source changes before claiming the current checkpoint verified.

## Verified native management checkpoint: 2026-10-07

- Required ordinary parallel tests pass: 251 passed, 39 native oracles ignored
  by the ordinary command. Log: `/data/cache/virtdisk-management-host-tests.log`.
- Explicit native parallel aggregate passes: 290 tests, zero failures and zero
  ignored. Log: `/data/cache/virtdisk-native-management-tests.log`. Includes new
  QEMU QCOW2 resize/flat VMDK checks and VirtualBox/QEMU VDI discard checks.
- VDI discard recovery covers 22 interruption boundaries with archived tail
  reconstruction, parent authorization, repeat replay and exact surviving bytes.
- Formatting, all-target/all-feature host Clippy and Windows x64 target Clippy
  with warnings denied pass after integration. This is not Windows runtime proof.
- Updated ten-target fuzz models pass eight replay/corpus tests, all target
  builds, formatting and locked Clippy. New measured smoke passes: ten targets, 101 iterations each, zero crashes
  and timeouts; source-change detection false and recorded source hashes verified
  against current files. Receipt:
  `target/fuzz-runs/20261007T122040Z-2/summary.json`.

Still required: broader capacity/discard and writable split profiles, internal
QCOW2 snapshot ownership/lifecycle, native VHDX bitmap updates, multi-file
publication/recovery, sustained fuzzing, native Windows/VMware acceptance, repair,
incremental tracking, benchmarks and remaining operation policy consolidation.
The complete implementation plan remains active.

## Snapshot reads and additional native management

- Native VDI capacity changes and VHDX dynamic whole-block discard now dispatch
  through `ImageWriter`; VHDX discard masks inherited bytes without promising
  host truncation. Allocated VDI arena growth now rotates payload units through bounded journal stages.
- Writable authorized split-flat VMDKs retain descriptor and every extent lock,
  preserve offset slices, and update CID before cross-extent writes.
- QCOW2 snapshot directory listing and immutable saved disk views pass native
  QEMU tests. Global ownership audits include active and snapshot owners; VM-state
  ownership and snapshot revert/deletion remain incomplete. CLI list/export tests pass,
  including saved/current byte separation and no-overwrite export.
- Common sparse creation supports all five families and retained writer locks.
- A snapshot-capacity native aggregate passed before the new CLI snapshot tests;
  log: `/data/cache/virtdisk-snapshot-capacity-native-tests.log`. Final aggregate
  and campaign receipts must be refreshed after ongoing implementation changes.

No complete-plan or native Windows/VMware runtime claim follows from this work.

## Native snapshot creation and read-only checks

- `Qcow2Writer::create_snapshot` and `ImageWriter::create_snapshot` publish
  native disk-only snapshots through the existing Linux redo protocol. Saved
  mappings remain immutable through active COW and validated writable reopen.
  Limits and QEMU/fault evidence are in `qcow2-snapshot-write.md`.
- Handle inspection reports retained snapshot count and creation capability;
  snapshot-bearing native capacity changes are explicitly unavailable.
- CLI snapshot creation accepts binary IDs/names through hex input and rejects
  duplicate or malformed IDs without mutation; list/export
  preserve binary IDs through hex output and exact saved-state selection.
- `check_image` and CLI `check` audit recognized ownership, authorized chains
  and optional current logical payload accessibility with explicit scope.
  Opaque optional extension contents and content authenticity are not verified.
- Allocated standalone dynamic VDI growth relocates its arena by bounded
  allocation-unit rotations. Fault cuts cover completed prefixes, prezeroing and
  final publication; `vdi-resize.md` records profile and performance limits.

Aggregate verification and fresh stateful snapshot campaigns are pending after
these changes; historical receipts do not establish the current source state.

## Final native snapshot test checkpoint: 2026-10-07

- Required ordinary tests pass: 296 tests, zero failures, 48 native oracles
  skipped by the ordinary command. Log:
  `/data/cache/virtdisk-native-snapshot-final-host-tests.log`.
- Explicit native aggregate passes: 344 tests, zero failures and zero ignored,
  including QEMU snapshot/COW checks and strengthened allocated VDI rotation
  acceptance with VirtualBox/QEMU. Log:
  `/data/cache/virtdisk-native-snapshot-final-aggregate-tests.log`.
- Production formatting, Linux all-target/all-feature Clippy, Windows x64
  all-target/all-feature target Clippy, and fuzz Clippy pass with warnings denied.
- Fourteen fuzz replay/corpus tests pass, including native snapshot models and
  a valid shared-L1 read-only fixture. Log:
  `/data/cache/virtdisk-native-snapshot-final-fuzz-tests.log`.
- Shared active L1 clusters now reject at writable open. Regression fixtures
  include shared unused L1 tails and retain valid immutable saved views. The
  previous journal already rejected the tested unsafe COW proposal before
  mutation; this change makes the writable profile explicit at opening.

Fresh measured ten-target Honggfuzz smoke passes: 101 iterations per target,
zero crashes/timeouts and source-change detection false. All recorded production,
harness, manifest, lock, configuration and runner hashes match the checkpoint.
Receipt: `target/fuzz-runs/20261007T130849Z-2/summary.json`.
This is bounded smoke evidence; sustained campaigns remain required.
Snapshot deletion/revert design is recorded in
`qcow2-snapshot-lifecycle-design.md`; the subsequent checkpoint implements
those operations for the bounded standalone Linux profile.
Native Windows/Hyper-V/VMware runtime acceptance, sustained fuzzing, remaining
profiles, operation budgets, repair, tracking and benchmarks remain open.


## Native snapshot lifecycle and capacity checkpoint

The bounded standalone Linux profiles now implement QCOW2 snapshot deletion
and revert, hosted sparse VMDK native capacity changes, and dynamic VHDX native
capacity changes. Common writer dispatch, profile-specific capability reporting,
and CLI commands exercise these implementations. Native resize dispatch covers
all five format families; each container still has explicit profile limits.

- QCOW2 deletion preserves surviving states and raw directory metadata; revert
  restores the selected capacity and bytes using a private active L1. Eight
  lifecycle integrations and 58 recovery cuts cover shared ownership, missing
  IDs, last-state deletion, compressed-state refusal and canonical empty revert.
- VMDK growth prepares expanded primary/redundant table coverage and relocates
  intersecting live grains before capacity publication. Shrink and regrowth
  clear hidden data. Tests include 32 GiB sparse growth and native QEMU recovery.
- VHDX capacity transactions reuse the native metadata-sector redo protocol,
  initialize new BAT entries and clear removed mappings within retained BAT
  coverage. Fault tests cover fresh/reused log epochs and torn size metadata.
- Required locked host tests pass: 324 passed, 54 independent-tool tests ignored.
  The independent QEMU/VirtualBox aggregate passes all 378 tests with no ignored
  tests. Logs: `/data/cache/virtdisk-lifecycle-resize-host.log` and
  `/data/cache/virtdisk-lifecycle-resize-native.log`.
- Rust formatting, Linux all-target/all-feature Clippy and Windows x64 MSVC
  all-target/all-feature Clippy pass with warnings denied. Fuzz formatting,
  all-target Clippy and all 16 deterministic replay/corpus tests pass.
- Stateful fuzz models now cover native VDI/VMDK/VHDX capacity changes and
  QCOW2 snapshot create/revert/delete with independent saved byte models,
  missing IDs, ID reuse, immutable parent checks and exact refusal invariants.

See `qcow2-snapshot-lifecycle-design.md`, `vmdk-resize.md` and `vhdx-resize.md`
for mutation, recovery and resource bounds. This checkpoint does not close
broader profile support, native Windows/Hyper-V/VMware runtime acceptance,
sustained fuzzing, operation budgets, repair, tracking or benchmark gates.

Fresh Honggfuzz smoke receipt:
`target/fuzz-runs/20261007T134053Z-2/summary.json`. All ten instrumented targets
pass 101 iterations each with zero crashes/timeouts. Source-change detection is
false, and all 60 recorded source/configuration hashes match the verified
checkpoint. This bounded campaign does not substitute for sustained fuzzing.

The next TDD increment is native VMDK discard. The new common-writer behavior
test `common_vmdk_discard_releases_native_mapping_and_preserves_neighbors`
fails with the existing explicit Unsupported result before implementation.
Its required behavior is native zero-readable deallocation, unchanged adjacent
bytes, truthful capabilities, and exact reopened logical content. This new
red test postdates the verified checkpoint above; it is not a claim that the
current in-progress worktree passes the aggregate suite. Native VHDX partial
sector-bitmap writes are the parallel implementation increment.


## Native trim and partial-sector work in progress

The VMDK common-writer and CLI tests first failed with native discard unavailable,
then passed after native hosted-sparse discard and capability dispatch were
implemented. The CLI test opens an authorized child, trims a complete grain,
reopens, performs a partial write and verifies zero neighbors, inherited bytes
outside the grain, and exact immutable parent bytes. Strict unaligned requests
and invalid ranges preserve the container before valid discard.

Focused logs: `/data/cache/virtdisk-vmdk-discard-common.log` and
`/data/cache/virtdisk-vmdk-discard-cli.log`. The native-writer model replay also
passes with native VMDK deallocation assertions, clipped final grains and
repeated discard/write/reopen/discard cycles in all three container writers:
`/data/cache/virtdisk-vmdk-discard-fuzz-model.log`.

VHDX native partial-sector mutation and QCOW2 compressed saved-state lifecycle
are parallel increments. Their fault tests, independent oracles and final
aggregate verification remain in progress. The earlier campaign receipt belongs
to its recorded frozen checkpoint; it does not validate these subsequent edits.
