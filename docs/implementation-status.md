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
  Versioned bounded manifests persist topology and selection, with explicit
  exact path authority and live graph revalidation when reopening. Optional
  graph-wide parser limits retain shared accounting through reopened and deferred
  ancestor readers and staged snapshot/rebase validation.
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
| VMDK | Hosted sparse v1/v2 and redundant tables; authorized flat/split descriptors; native hosted export; hosted payload writer; Linux journaled sparse allocation with redundant mapping recovery and authorized parent COW; standalone authorized monolithicFlat and twoGbMaxExtentFlat writes; bounded standalone hosted sparse resize; native whole-grain ZERO discard with parent masking; Linux standalone/backed split sparse writes with pinned parent manifests and v2 recovery binding; multi-table allocation using empty-extent arenas or unclaimed metadata padding | Parented flat, split creation/publication/native resize/discard, broader directory/metadata relocation, stream-optimized profiles, VMware-native acceptance |
| VHDX | Clean standalone fixed/dynamic reader; native dynamic export; immutable log replay; explicit locked native writable log recovery; payload writer with durable header GUID epochs; native logged sparse BAT allocation; authorized differencing, sector-bitmap inheritance and native partial-sector writes, native dynamic discard and bounded standalone dynamic resize; bounded real Windows process-termination and native replay interoperability | Broader resize profiles and host reclamation, physical power loss and broader supported-platform journal persistence |

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
| 5. VHDX | Reader/log replay/export, logged allocation and differencing COW implemented; Windows-produced children, clean/native replay and bounded actual process-termination acceptance pass; broader management/persistence gates incomplete |
| 6. VMDK | Hosted/descriptor chain readers and transactional hosted parent COW implemented; bounded standalone/backed split sparse COW and multi-table allocation implemented; split native management/publication and stream profiles incomplete |
| 7. Advanced management | Incomplete: broader native snapshot profiles, atomic image/manifest generations, in-place rebase/commit, additional native discard profiles, constrained repair, incremental tracking and benchmarks |

Windows x64 cross-compilation and target Clippy pass; this is compilation evidence
only. Nix inputs are pinned by the tracked `flake.lock`.

No milestone release gate should be marked complete from this ledger alone.
Required host checks use the default Nix shell and fuzz checks use `.#fuzz`.
Independent QEMU tests must be explicitly run with `--include-ignored`.
Actual Windows clean VHDX, native replay, Windows-produced children and bounded
actual process-termination evidence pass in the receipts recorded below.
Physical power loss, broader Hyper-V acceptance and native VMware runtime gates
remain. A bounded process-termination receipt does not complete those gates.
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


## Native trim, partial-sector and compressed-snapshot checkpoint

Native hosted-sparse VMDK trim now uses journaled ZERO entries, updates primary
and redundant tables, masks authorized parents, and keeps live zero masks
consistent through partial COW, repeated discard and native resize. Six focused
integration tests, 128 host recovery cuts and 18 native QEMU recovery cuts pass.
See `vmdk-discard.md`; mapping release does not imply host reclamation.

Native VHDX child writes now allocate and update sector bitmaps and publish
PARTIALLY_PRESENT, preserving arbitrary byte edges at 512/4096-byte sectors.
Four focused tests include shared bitmap owners, separate chunks, final sectors,
ZERO replacement and a 32 MiB payload crossing a bitmap-page boundary. Thirty-five
metadata interruption cases, three torn publication cases and 21 independent
native log protocol cuts pass. QEMU protocol acceptance on standalone fixtures
is distinct from actual Windows/Hyper-V differencing acceptance, which remains
unfulfilled. See `vhdx-partial-write.md`.

QCOW2 snapshot deletion handles compressed physical extents. Revert preserves
saved compressed storage while materializing private active grains within the
existing transaction budget. Nine lifecycle integrations including three QEMU
oracles and 104 recovery cuts pass. Oversized materialization refuses before
mutation. Active compressed payload writes remain unsupported.

Common writer/CLI trim tests and the CLI VHDX partial zero test pass. All 16 fuzz
replay/corpus tests pass, including clipped VMDK grains and repeated
trim/write/reopen/trim sequences. Runtime: 409.20 seconds for the expanded corpus.
Log: `/data/cache/virtdisk-native-trim-partial-fuzz.log`.

The final native aggregate exposed a process-spawn test race in
`tests/qcow2_writer.rs`: the stress reproduction failed on run 7 with WouldBlock
at immediate lock reopening. Integration tests now use the existing test-only
process-boundary guard to exclude subprocess spawning during writer lifetime
assertions; production locking is unchanged. Thirty guarded native runs pass.
Logs: `/data/cache/virtdisk-qcow2-lock-release-stress.log` and
`/data/cache/virtdisk-qcow2-lock-release-guarded.log`. Final whole-suite verification
and a fresh measured campaign are being completed after this correction.


Final required locked checks pass after the integration-test coordination fix:
340 host tests pass with 58 independent-tool tests ignored; the independent
QEMU/VirtualBox aggregate passes all 398 tests with no ignored tests. Linux and
Windows x64 all-target/all-feature Clippy and Rust formatting pass. Logs:
`/data/cache/virtdisk-native-trim-partial-final-host.log`,
`/data/cache/virtdisk-native-trim-partial-final-native.log`,
`/data/cache/virtdisk-native-trim-partial-final-clippy.log`, and
`/data/cache/virtdisk-native-trim-partial-final-windows-clippy.log`.

Fresh Honggfuzz receipt: `target/fuzz-runs/20261007T151013Z-2/summary.json`.
All ten targets pass 101 iterations each with zero crashes/timeouts, source
change detection false, and all 60 recorded source/configuration hashes match.
This is bounded smoke; sustained campaigns remain a separate release gate.

A prepared Windows VM base was discovered through the local vm-run tooling.
An isolated VM and independent Windows acceptance harness are the next native
gate attempt; availability or boot alone does not prove disk acceptance.


## Windows-native clean VHDX acceptance (2026-10-07)

Actual Windows 11 Pro, version 10.0.26200.8037, accepts all four prepared
4 MiB VHDX images: standalone and native partial-sector differencing children
at 512-byte and 4096-byte logical sectors, with 4096-byte physical sectors.
The native Microsoft provider opens, attaches read-only and detaches each image
successfully. Full surfaced logical SHA256 matches each manifest; all copied
children and parents retain their exact before/after file hashes. The isolated
VM uses a private overlay of the immutable prepared base with networking disabled.

The initial executable failed before main with Windows status 0xC0000135. A
command-scoped static CRT build removes the dynamic CRT dependency, and its
PE imports contain only Windows system DLLs. Harness formatting, five host
tests, host Clippy and Windows target Clippy pass. This harness is an independent
Cargo workspace; production and fuzz sources remain at the preceding receipt.

Checked-in evidence: `evidence/windows-vhdx-clean-20261007.json`. Full execution
receipt: `/data/cache/virtdisk-native-windows/receipts/20261007T171445Z.json`.
See `windows-acceptance.md` for reproducible preparation and native API checks.
The original manifests used library-derived hashes. A separate byte-array
model of every fixture patch now independently reproduces all four native
logical hashes; its provenance is retained in the checked-in evidence. Dirty
native replay, Windows-produced children and interruption/power-loss tests
remain separate gates. These four results do not close the complete implementation plan.


Dirty fixture TDD first failed for missing replay artifacts, then passed for all
eight independent-model recovery cases. They reconstruct the actual writer's
durable-redo-before-metadata boundary, preserving a real redo record and
restoring pretransaction target sectors. This is deterministic reconstruction,
not captured OS power loss. All six harness host tests, formatting, host and
Windows Clippy pass.

First native dirty run: all four library-recovered standalone/child images
at 512/4096 sectors pass Windows full-content validation with immutable parents.
All four direct native-replay attempts fail at OpenVirtualDisk with code 5,
before attachment; original fixture hashes remain unchanged. The harness's
mode-specific access contract is being investigated before rerunning. Evidence:
`evidence/windows-vhdx-dirty-initial-20261007.json`; full receipt:
`/data/cache/virtdisk-native-windows/receipts/20261007T171815Z.json`.


After the harness requested leaf backing write access for NativeReplay
(`ATTACH_RW`, RWDepth 1), all eight dirty cases pass on actual Windows:
four direct Windows-native replay and four library-recovered acceptance runs.
The surfaced disk remains explicitly READ_ONLY with no drive letter, and
all parent file hashes remain exact. Native replay changes only the disposable
leaf's metadata as expected; full logical hashes match independent models.
The new access-policy regression brings the isolated harness to seven tests;
host and Windows Clippy, static CRT build and formatting pass.

Evidence: `evidence/windows-vhdx-dirty-20261007.json`; full receipt:
`/data/cache/virtdisk-native-windows/receipts/20261007T171958Z.json`.
This establishes native redo interoperability for reconstructed publication
cuts, not actual process interruption, host power loss, Windows-produced partial
children, broader allocation profiles or all Windows writer persistence.

QCOW2 boundary tests now verify reachable refcount-block growth at 2 GiB: new
private payload and L2 mapping, new block self-reference, exact old/shared/new
counts, preserved saved state, and deletion/revert after growth. Two host tests
and one independent QEMU check/convert pass in 8.00 seconds. No production or
fuzz implementation changes were necessary. See `qcow2-refcount-growth.md`.
Refcount-table relocation remains unsupported in the current bounded profile.


## Windows-produced partial children and native locator regression

A separate producer uses the Microsoft provider to create real differencing
children of pinned copied parents, attaches only its newly created child, writes
three aligned logical sectors, flushes and detaches, then performs native
read-only full-hash verification. Actual Windows runs pass at 512/4096 logical
sectors, preserving exact parent hashes. Each child contains two native
PARTIALLY_PRESENT payload entries and a FULLY_PRESENT sector bitmap, with
2 MiB payload blocks.

Import exposed a real reader incompatibility: ordinary Windows drive-rooted
absolute parent hints were rejected despite an explicitly authorized relative
parent. A failing parser regression and the actual downloaded fixture failed
first. `src/vhdx/parent.rs` now accepts conventional drive-rooted hints while
retaining explicit authorization, opened parent identity and linkage checks.
Drive-relative paths, streams, device paths and unsupported namespaces remain
rejected. The final fixture transfer uses conventional single Windows separators;
the earlier experimental doubled namespace was not accepted by broadening the
parser.

`tests/vhdx_windows_native.rs` preserves exact compressed Windows-produced files
and provenance in `tests/fixtures/vhdx/windows-11-26200`. It verifies native BAT
states, every byte against independently modeled writes, unchanged source files
and failure without parent authorization for both sector sizes. The regression
passes after the locator fix. New VHDX chain fuzz seeds cover ordinary drive
hints and rejected alternate-stream/device hints.

Evidence: `evidence/windows-vhdx-produced-20261007.json`; full receipt:
`/data/cache/virtdisk-native-windows/receipts/20261007T172747Z.json`. Production
and harness sources are now frozen for required checks and a fresh campaign.
These results close the bounded Windows-produced partial-child parser gate;
actual process interruption/power loss and broader native profiles remain.


The owned isolated Windows VM was shut down through QMP system_powerdown after
all producer/attachment jobs completed and detached. The supervising vm-run
process exited successfully; its private disk overlay remains available for
future native runtime gates. The prepared base remains immutable.

The next split sparse VMDK increment has a concrete staged coordinator design in
`vmdk-split-transactions.md`. Complete participant identity/digest validation must
precede every recovery mutation, with bounded markers and aggregate budgets.
This is a reviewed design, not an implemented split sparse write capability.


Final full QEMU/VirtualBox aggregate after the native locator regression passes
all 403 tests with zero failed or ignored tests. Formatting and Linux/Windows
all-target/all-feature locked Clippy pass; the separate fuzz workspace Clippy
also passes. Aggregate log: `/data/cache/virtdisk-windows-native-final-tests.log`.

The first new measured campaign, `target/fuzz-runs/20261007T173150Z-2/summary.json`,
reports three VMDK writer timeouts and zero crashes while both full regression
suites were executing concurrently. Its receipt is failed and retained; it
does not validate the new checkpoint. Source change detection is false and all
60 recorded source/configuration hashes match. The full deterministic fuzz suite
is being completed before an isolated repeat under the unchanged five-second
case timeout.


All 17 deterministic fuzz replay/corpus tests pass after the native locator
change. Full corpus runtime: 508.51 seconds while overlapping the native
aggregate and initial instrumented campaign. Log:
`/data/cache/virtdisk-windows-native-final-fuzz-tests.log`. The isolated
instrumented repeat has started after those suites completed.


The isolated Honggfuzz repeat passes all ten targets at 101 iterations each,
with zero crashes/timeouts and unchanged sources. Fresh passing receipt:
`target/fuzz-runs/20261007T174104Z-2/summary.json`. All 60 source/configuration
hashes match. The failed concurrent receipt remains retained independently;
the passing repeat does not erase that evidence or prove the timeout cause.
This is bounded smoke, not sustained fuzzing or security qualification.

This checkpoint includes 403 passing host/native-tool tests, 17 passing fuzz
replay tests, successful formatting and Linux/Windows/fuzz Clippy, and the
bounded Windows clean, reconstructed native replay and Windows-produced child
evidence above. The full implementation plan remains open, particularly split
sparse writes, Windows hosted journal persistence, actual power-loss evidence,
advanced profiles, constrained repair, tracking and benchmarks.


### Split sparse VMDK allocated-grain checkpoint and Windows mutation evidence

Linux standalone `twoGbMaxExtentSparse` descriptors now support allocated-grain
writes and zero fallback across explicitly authorized extents. A coordinated
transaction validates the complete participant set before recovery modifies any
file, publishes participant markers, and persists the descriptor CID before
payload. Native allocation, backed COW, discard, resize and creation remain
unsupported for this profile. See `vmdk-split-transactions.md` for bounds and
recovery ordering. Integration and common writer tests pass. All seven backend recovery tests
pass, including every publication cut; fresh aggregate verification is pending.

Actual Windows Rust writer execution passed all twelve steady-state cases:
existing base/child mutation, fresh image creation and derived image creation
for both input sector sizes. Independent full-content models matched native
read-only attachment hashes; competing writers refused, native attachment left
clean files unchanged, and source parents stayed immutable. Native resize and
discard refused without changing files. Evidence is retained in
`evidence/windows-vhdx-mutation-20261007.json` and the referenced full receipt.
Actual process interruption and power-loss gates remain unfulfilled.

The earlier 403-test/17-replay/ten-target passing receipt records the preceding
frozen source checkpoint. The new split writer and fuzz models require fresh
checks and campaign evidence; that earlier receipt does not validate these edits.


The twelve-case Windows mutation VM was subsequently shut down gracefully through
QMP; its supervising process exited successfully. The new frozen split checkpoint
passes formatting and Linux/Windows locked all-target Clippy. Four added split
fuzz model tests pass, with 21 replay tests in the aggregate suite now being
verified. Default temporary storage showed journal stalls, so this verification
uses an explicitly recorded cache-backed temporary directory without increasing
the five-second fuzz case timeout.


Fresh full QEMU/VirtualBox aggregate passes all 418 tests, with zero failures or
ignored tests. Formatting and Linux/Windows/fuzz locked all-target Clippy pass.
The 21-test fuzz replay aggregate and isolated instrumented campaign remain
pending; source files remain frozen until their receipts are collected.


All 21 deterministic fuzz replay/corpus tests pass (515.42 seconds). The fresh
instrumented smoke campaign now runs in isolation, with cache-backed `TMPDIR`
and the unchanged five-second case timeout. Its receipt remains pending.


The fresh isolated Honggfuzz campaign passes all ten targets at 101 iterations
each, with zero crashes/timeouts and unchanged source hashes. Receipt:
`target/fuzz-runs/20261007T182206Z-2/summary.json`. Every recorded source/configuration
hash matches the frozen checkpoint. The VMDK writer target includes the new split
models; its measured slowest case is 2687 ms under the five-second limit.

This bounded checkpoint has 418 passing host/native-tool tests, 21 passing fuzz
replay tests, successful formatting and Linux/Windows/fuzz Clippy, and the twelve
actual Windows steady-state writer cases recorded above. Split grain allocation
and backed COW are still pending, as are the broader remaining plan gates.


The next TDD increment now implements standalone split sparse grain allocation
into existing tables. Meaningful missing-grain tests first failed with
`Unsupported`; cross-extent allocation, zero fallback and independent reopened
byte models now pass through both the direct and common writer APIs. Fuzz modes
4/5/6 cover allocated writes, existing-table allocation and unchanged missing-table
refusal. Six scoped model tests pass. Recovery and fresh aggregate/campaign gates
remain pending; the preceding 418/21-test receipt describes the earlier frozen
allocated-only checkpoint. Grain-table creation, backed COW and native management
remain unsupported for split sparse images.


Existing-table split allocation source is now frozen. Scoped evidence passes nine
host backend tests and all 29 native QEMU recovery cut cases; eight integration
tests pass, including three independent QEMU conversions and late alignment,
aggregate-growth and foreign-growth refusals. Formatting and Linux/Windows
locked all-target Clippy pass. Fresh full aggregate/replay logs are being written
to `/data/cache/virtdisk-split-allocation-full-tests.log` and
`/data/cache/virtdisk-split-allocation-fuzz-tests.log`; the campaign follows those
terminal results. This records simulated persistence cuts with native recovery,
not actual process termination or power loss.


The fresh full QEMU/VirtualBox allocation aggregate passes all 427 tests, with
zero failed or ignored tests (61 result groups). The full log is retained at
`/data/cache/virtdisk-split-allocation-full-tests.log`. Formatting and
Linux/Windows Clippy pass. The 23-test deterministic fuzz aggregate is still
running; instrumented smoke follows its terminal result.


All 23 deterministic fuzz replay/corpus tests pass (517.47 seconds), with the
full log retained at `/data/cache/virtdisk-split-allocation-fuzz-tests.log`.
Main and fuzz formatting and Linux/Windows/fuzz Clippy pass. The fresh isolated
instrumented campaign is now running with cache-backed `TMPDIR` and unchanged
five-second cases; its log is `/data/cache/virtdisk-split-allocation-campaign.log`.


Fresh allocation Honggfuzz smoke passes all ten targets at 101 iterations each,
with zero crashes/timeouts and unchanged sources. Receipt:
`target/fuzz-runs/20261007T185719Z-2/summary.json`. All 63 source/configuration
hashes match. The VMDK writer target's slowest measured case is 2204 ms under
the unchanged five-second limit; modes cover existing-table hole allocation and
missing-table unchanged refusal.

This frozen allocation checkpoint has 427 passing host/native-tool tests, 23
passing deterministic fuzz tests, main/fuzz formatting and Linux/Windows/fuzz
Clippy. It remains bounded smoke, not sustained fuzzing or security qualification.
The implementation plan stays open, including table creation, split backed COW,
actual Windows process termination/power loss, native VMware and advanced gates.


Actual Windows process-interruption gate passes 100 cases: 84 real worker
terminations and 16 controls/traces, across 512/4096 sector sizes, standalone and
differencing images, fresh and retained writer states. The harness acknowledges
successful real owned-leaf FlushFileBuffers calls, then its controller terminates
the blocked worker with exit code 0x56444355. Root independently rebuilt every
4 MiB old/new model and verified both library recovery and native replay hashes,
native open/attach/detach results, geometry and clean recovered leaf immutability.
Per-sector observed trace counts are 10/1/18/9 for standalone fresh/retained and
child fresh/retained. The retained standalone case is an already private payload
flush, not a new mapping transaction.

Evidence: `evidence/windows-vhdx-process-kill-20261007.json`; full receipt:
`/data/cache/virtdisk-native-windows/receipts/20261007T190438Z.json`. The exact
static executable and harness source archive are preserved under the owned
artifacts directory. Exact raw child/parent files from child-fresh cut 14 are
retained in `tests/fixtures/vhdx/windows-process-kill` for both sector sizes.
These are actual terminated-worker files, without reconstructed redo or locator
edits. Native replay matched the new model and changed the dirty copied leaf.
Physical power loss, arbitrary multi-block atomicity and servicing remain open.

The daemon restarted after all runtime jobs and fixture downloads completed.
On resume, the owned VM's QMP socket is absent; its private state is retained.
That interruption provides no additional power-loss evidence. Missing-table VMDK
implementation resumes from partial local edits; the last full verified source
checkpoint remains the 427/23-test allocation receipt above.


Both actual Windows killed-child fixture regressions now pass on the host.
Recovered views preserve exact raw child/parent bytes; missing-parent authority
refuses without mutation; authorized native recovery produces the independent
new model and repeated recovery is idempotent. The fixture test uses both sector
sizes. These checks are additional to the prior frozen checkpoint; a fresh
aggregate will include them after the missing-table increment is frozen.


Fuzzing is paused at the user's explicit request because of host lag. The
lingering default-temporary-filesystem replay was terminated (exit 143); no
further fuzz campaigns, replay suites or timing runs should start until the user
resumes fuzzing. Preserve completed scoped results and prior campaign receipts;
cancellation is not a passing fresh campaign gate. Continue non-fuzz implementation
and lightweight verification within the user's remaining authorized scope.

After the pause, a single-job, serial rerun of `tests/vhdx_windows_native.rs`
passes both native-produced and actual terminated-child fixture regressions
(2 tests, 0.49 seconds). This verifies the current host fixture behavior only;
the completed native Windows receipt above remains the runtime evidence.
The missing-table full aggregate log
`/data/cache/virtdisk-split-tables-full-tests.log` has no terminal aggregate
result and must not be cited as a passing checkpoint. The corresponding fuzz
aggregate is also incomplete and remains paused.

The next split VMDK increment uses unclaimed metadata space below unchanged
overhead to install a missing grain table in an extent that already contains
mapped data. Header, complete descriptor reservation, directory sectors and
existing complete grain tables must remain protected. Replacement table bytes,
directory pointers and initialized appended payload belong to one complete-set
transaction; full-request space and budget refusal must precede mutation.
This increment is in progress and has no acceptance claim yet.

The metadata-padding increment is now implemented. Its first regression failed
with `Unsupported` before implementation and now passes while preserving mapped
payload, unchanged overhead and initialized logical neighbors. Forty primary and
redundant recovery cut cases pass (6.97 seconds). Four serial integration cases,
including independent QEMU check/conversion and full-directory-sector protection,
pass (0.84 seconds). Additional refusal checks cover unchanged descriptor and
both extents, absent sidecars, and zeroing. Later calls reserve a fresh gap instead
of reusing a previously published table. Formatting and single-job locked
all-target/all-feature Clippy pass.

Each call still creates at most one missing table pair per extent; directories
must exist, physical tails must be aligned, and metadata relocation, split backed
COW and native management remain unsupported. These are simulated recovery cuts
and QEMU interoperability, not native VMware or power-loss qualification.
The required standard Rust tests are running serially with one build job;
their log is `/data/cache/virtdisk-padding-standard-tests.log`. No fresh fuzz
acceptance is claimed while fuzzing is paused.

The first serial standard run exited 101 at the split integration binary:
`existing_table_prefix_before_missing_table_refuses_before_cid` still expected
`Unsupported`, but the new padding path accepted its mapped-prefix fixture.
Earlier binaries and the new cut tests passed. This result is retained as a
failure, not a passing aggregate; the boundary regression must verify the new
supported write/zero behavior and exact neighboring bytes before a fresh run.

The stale boundary expectation has been replaced with an independent 33 MiB
write/zero model, unchanged overhead, preserved mapped physical neighbors,
validated new directory/table entries, repeated immutable reopen and absent
pending markers. All 15 split integration tests now pass serially, including
five QEMU oracles (4.40 seconds). Core sources did not change in this correction;
formatting passes. A fresh serial required run is recorded in
`/data/cache/virtdisk-padding-standard-tests-final.log`; its terminal result is
pending. The earlier failed log is retained separately.

Next reviewed split increment: remove the one-missing-pair-per-extent call
restriction with a complete-call, bounded table plan. Reserve distinct protected
padding holes for allocated extents. For an empty extent requiring a new arena,
publish all required zero-initialized tables, coalesced directory patches and
the final overhead in one metadata-only complete-set transaction before any
payload append. This avoids moving overhead over a newly allocated prefix.
Preparatory transaction costs must be included in whole-call budgets; committed
metadata with unchanged logical bytes is a possible I/O-failure state. Cached
overhead and table ownership update only after commit. Required red regressions
cover two missing pairs in one tiny boundary write, an existing-table prefix in
an empty extent, and later reuse of the new arena's unclaimed padding. This is
a reviewed design, not an implemented capability.

The frozen padding checkpoint's fresh serial standard run exits successfully:
373 passed, zero failed, 65 optional tests ignored across 61 result groups.
Log: `/data/cache/virtdisk-padding-standard-tests-final.log`. Final locked
all-target/all-feature Clippy passes after the boundary test correction. The
separate split integration run includes all five QEMU oracles and passes all
15 tests. The standard run does not establish the other 60 ignored optional
gates; no fresh aggregate native-tool or fuzz result is claimed.

The following backed split COW increment has a concrete design in
[vmdk-split-parent-cow.md](vmdk-split-parent-cow.md). It requires retained read-only
parent pins and explicit immutable dependency bindings in a versioned journal;
existing unchanged writable participants cannot stand in for parents because
they still receive markers and cleanup. The configured Rust 1.99 standard
library provides nonblocking shared file locks; no toolchain change is needed.
The design includes exact parent descriptor/extent validation before recovery
mutation, shared budgets, inherited-grain/ZERO semantics and TDD/QEMU gates.
Backed split writes remain unsupported pending implementation and verification.

Multiple-table TDD reds are now established: the allocated two-pair boundary
write, empty two-pair boundary write, and empty existing-table prefix all fail
with `Unsupported` at their two-byte writes. Targeted runs exited 101 as
expected (0.06–0.12 seconds), before production changes. Tests are
`multiple_missing_tables_share_complete_call_plan`,
`multiple_missing_empty_tables_share_arena`, and
`existing_empty_prefix_is_above_prepared_metadata` in the split integration
file. Implementation and recovery checks are in progress; these failures are
requirements evidence, not acceptance evidence.

The multiple-table increment is implemented and frozen. All 20 split integration
tests pass serially, including the five existing QEMU gates (7.04 seconds).
A new independent QEMU conversion gate compares complete logical arrays for
allocated-padding and empty coalesced-arena cases, both with redundant table
pairs (0.63 seconds). The phase test passes 96 probes (26.25 seconds), separately
checking metadata-only preparation, payload-prefix recovery and cache publication.
Preparation exposes 15/16 interruption boundaries for primary/redundant cases;
payload publication after preparation exposes 11/12. An initial generic count
assertion failed because payload publication no longer needs a CID record;
the phase-specific counts and every logical/cache assertion now pass.

Whole-call planning owns its cache reservation and reserves every table hole
before mutation. Empty multi-table or existing-table-prefix calls prepare one
coalesced-directory arena before payloads; committed empty metadata with unchanged
logical bytes is an allowed failure outcome. Mutable cached overhead enables
later reuse of a prepared arena. A no-original-padding regression proves this
reuse, and zeroing across chunks prepares once. Formatting and single-job
all-target/all-feature Clippy pass. The fresh serial required run is underway
at `/data/cache/virtdisk-multiple-tables-standard-tests.log`. Fuzzing remains
paused, and the broader plan remains open.

The frozen multiple-table checkpoint's standard suite exits successfully:
379 passed, zero failed, 66 optional tests ignored across 61 result groups.
Log: `/data/cache/virtdisk-multiple-tables-standard-tests.log`. All 110 archived
runtime/test/fixture/build-pin hashes still match at the terminal result.
Archive and manifest:
`/data/cache/virtdisk-multiple-tables-20261007-sources.zip` and its `.json` receipt.
This excludes fuzz and the independent Windows acceptance harness. The six
separately executed QEMU gates cover split allocation/table paths; the other
60 ignored optional gates remain outside this fresh aggregate.

The next inspection increment is specified in
[inspection-container-sizes.md](inspection-container-sizes.md). It preserves
existing primary-file `container_size` semantics and adds explicit own-container
aggregate size, including full flat extent prefixes/tails and excluding parents
and sidecars. Retained writer facts must update after allocation and capacity
changes; unavailable facts must remain distinct from zero.

The inspection increment is now implemented. `ImageInspection` exposes optional
primary and own-container aggregate sizes; CLI info retains primary-file
`container_size` and adds nullable `container_set_size`. VMDK aggregates physical
descriptor/extent lengths, including flat prefixes and tails, and excludes
parents and journals. Writer inspection uses retained handles after allocation,
resize and discard. A meaningful raw-size behavioral failure preceded getter
wiring. Eleven inspection tests, two targeted CLI tests, formatting and targeted
Clippy passed. Root all-target/all-feature Clippy also passed. The fresh required
serial suite exited successfully: 386 passed, zero failed, 66 optional tests
ignored across 61 result groups. Log:
`/data/cache/virtdisk-inspection-sizes-standard-tests.log`. Final formatting and
x86_64 Windows MSVC all-target/all-feature Clippy also passed; the latter is a
compile check, not Windows runtime acceptance. Its log is
`/data/cache/virtdisk-inspection-sizes-windows-clippy.log`. The newly prepared
`tests/vmdk_split_parent.rs` was added after suite discovery and is explicitly
outside this passing checkpoint; backed split COW remains unimplemented.
Fuzz campaigns and replay remain paused by user instruction.

Backed split sparse VMDK implementation is in progress after two meaningful
behavioral failures: authorized external-descriptor writer opening failed after
the reader established valid inheritance, and the old set decoder rejected an
independently encoded dependency journal. The first two integration tests now
pass, including cross-extent/final-partial COW, ZERO masks, partial zeroing,
unchanged parent bytes, sibling preservation and writer shared-lock exclusion.
Six pinned-graph tests pass for typed topology, read-only retained locks,
authorization, child aliases, replaced or resized sources, foreign parent
markers, caller budgets and exact frozen child sources. Three existing reader
chain tests also pass (one optional native test remains ignored).

`VDTXSET2` encodes immutable physical dependencies and typed ancestor topology;
standalone `VDTXSET1` bytes are preserved. Seven scoped journal tests pass,
including checksum-valid hostile topology/roles and five changed-parent recovery
cases that preserve every child/parent/sidecar byte and directory entry. A frozen
legacy decoder extracted from the archived multiple-table checkpoint accepts
legacy journals and refuses V2; fixture and extraction hashes are retained in
`tests/fixtures/vmdk/legacy-set-codec.json`. Logs:
`/data/cache/virtdisk-backed-codec-red.log` and
`/data/cache/virtdisk-backed-codec-tests.log`. This is a scoped progress receipt,
not full current-tree acceptance. The interruption matrix, independent native
QEMU checks and fresh required checks remain outstanding. No fresh fuzzing ran.

Subsequent bounded split gates passed: four host recovery tests include 96
interruption cases, authorization refusal, changed parent refusal and authentic
unbound V1 no-cleanup refusal. QEMU 10.2.4 accepted 144 recovered full-content
conversions, and a separate unchanged test passed with actual QEMU-produced
131072-byte parent/child files, backing-chain info and native version-2 ZERO
mappings. This producer test first failed on the reader's version-1-only gate;
versions 1/2 now use the same bounded uncompressed layout with the existing
feature, ownership, dirty-state, compression and newline checks. Multi-generation
graph tests cover sparse and flat ancestors, complete manifests and aliases.
A 32 KiB cache regression failed meaningfully before adding a temporary 64 KiB
digest working-memory reservation; its green case verifies reservation release.

The filesystem writer release plan is now being implemented as version 0.2.0.
Only path-package version entries changed in the lockfiles; dependency pins are
preserved and fuzzing remains paused. The common `ImageWriter::open_chain`
backed-split dispatch regression failed meaningfully and now passes with all
eight common-writer tests and three split-parent integration tests. The first
release-wide suite was stopped at exit 130 after this API audit finding and is
not a successful checkpoint. Corrected-source formatting and all-target,
all-feature locked Clippy passed. The corrected fresh serial suite is running at
`/data/cache/virtdisk-0.2.0-standard-tests-final.log`; its terminal result is not
yet claimed. Package listing succeeded, but package verification, publication
and registry-consumer evidence remain pending.

Version 0.2.0 is published. Corrected required checks finished with 406 passed,
zero failed and 68 optional tests ignored across 62 result groups. Formatting,
Linux all-target/all-feature Clippy and x86_64 Windows MSVC Clippy passed.
Publication dry-run packaged and verified 158 files; registry upload completed.
The downloaded registry artifact checksum is
`ed266f4c5cae3fba8b0470b42cfe43dd379c12ecfaab31a52f56456ce0fe4f8d`;
all comparable packaged files matched owner sources at verification.

The actual storage bridge copied into an isolated consumer passed five
default-library API/session/failure tests against registry virtdisk 0.2.0 with
no virtdisk path patch and empty resolved features. Its independent native QCOW2
gate passed full raw conversion, structural check and unchanged-source comparison.
Raw/QCOW2 small debug timing samples include eight sequential operations, 64
random operations, flush and exact whole-image reopen validation; no sustained
performance acceptance threshold is claimed. Partmgr remains a frozen local
source snapshot for this isolated bridge. Its original checkout was not edited.
The release plan's owner publication gate is closed; consumer-owner manifest
migration and broader native/performance acceptance remain separate work.
Exact source, package, lock, API, feature and log hashes are retained in
[filesystem-writer-release-0.2.0.json](evidence/filesystem-writer-release-0.2.0.json).
Fuzzing remains paused; the full image-management implementation plan remains open.

## Remaining-work priority: 2026-10-08

The user selected common API first, then snapshots and chains, followed by
QCOW2, VHDX, VDI and VMDK. The delivery-plan priority section supersedes the
original milestone order without renumbering historical evidence. Common
snapshot/chain contracts use existing supported writer profiles; missing
format-specific machinery follows in its format stage. Test-driven development
and acceptance gates remain required. Fuzz campaigns and replay remain paused.

## Common API operation error context: 2026-10-08

Added public `OperationError` with private fields and typed accessors for the
attempted operation, selected format and optional logical offset/length. Common
writer handle operations preserve their `io::Result` signatures, original
`ErrorKind` and underlying error source. Context is collected without inspection,
path reopening or additional I/O. No rollback or recovery state is inferred.
Opening/creation and concrete writer error contracts remain unchanged.

The initial public API test failed to compile because `OperationError` was
absent, then passed after implementation. Two public behavioral tests verify
range/source/kind preservation and unchanged raw contents after refused native
snapshot creation and shrink. Existing writer/recovery tests also pass.
Required locked all-feature serial tests: 408 passed, zero failed, 68 ignored;
log: `/data/cache/virtdisk-common-api-tests.log`. Formatting and warnings-denied
Linux all-target/all-feature Clippy and Windows x64 MSVC target Clippy pass.
Windows compilation is not runtime acceptance. No fuzzing or fuzz replay ran.

See [common-operation-errors.md](common-operation-errors.md). Common opening and
recovery policy consolidation, operation limits and progress/cancellation remain
open; this increment does not complete the common API stage or the full plan.

## Explicit common writer opening policy: 2026-10-08

Added `WriterOpenOptions`, `RecoveryPolicy` and typed `RecoveryRequired`
refusals through `ImageWriter::open_with_options`. Defaults require a standalone
image with recovery forbidden. Explicit authorization selects parent/extent
resolution; VDI retains ordered parent paths. Existing opening APIs preserve
their per-format recovery behavior. Options use private fields and typed policy
choices rather than ambiguous boolean arguments.

Clean-only sidecar profiles check pending evidence under retained writer locks
and never call replay routines. Split sparse VMDK retains all participant locks
before checking transaction evidence. VHDX validates the native recovered view
and rejects pending redo before writes; authorized replay transfers the same
locked file directly into the writer. Recovery failure can leave partially
replayed metadata, including when no writer is returned. Flat VMDK recovery
remains unsupported. Native platform/profile bounds are unchanged.

TDD evidence includes the absent-API compile failure, followed by a behavioral
failure when the initial implementation delegated to the old implicit recovery
path without enforcing the selected policy. Public tests now cover clean-only
refusal, clean recovering opens across all five families, lock retention,
native VHDX full-image recovery and idempotent reopen. Existing QCOW2, VDI,
hosted VMDK and split VMDK interruption cases now also exercise common opening:
exact directory snapshots survive refusal, then explicit recovery reproduces
the byte model. Authorized parent/COW integration uses the new options API.

Final required locked all-feature serial tests pass: 411 passed, zero failed,
68 optional tests ignored across 64 result groups. Log:
`/data/cache/virtdisk-opening-policy-tests-final.log`. Formatting and
warnings-denied Linux and Windows x64 MSVC all-target/all-feature Clippy pass.
The separate QEMU VHDX oracle passes structural check and complete 1 MiB logical
conversion after common recovering open; log:
`/data/cache/virtdisk-opening-policy-qemu.log`. This does not establish Windows
runtime or physical power-loss behavior. Fuzz campaigns and replay remain paused.

These are development changes after registry 0.2.0, not a new published release.
See [writer-opening-policies.md](writer-opening-policies.md). Operation limits,
progress/cancellation, reader policy consolidation and CLI integration remain
in the common API stage; the complete implementation plan remains open.

## Common operation contexts: 2026-10-08

Added reusable `OperationContext` with private, validated `OperationLimits`,
typed resource-limit/cancellation errors, cumulative usage and a borrowed
`FnMut` progress observer returning `ControlFlow`. Context-aware copy, SHA-256
hashing and logical comparison preflight the complete requested byte/I/O budget
before callbacks, allocation or I/O. Scratch is fallibly allocated, at most
64 KiB per buffer and 128 KiB combined for comparison, preserving legacy I/O
granularity. Existing APIs delegate to the shared implementations; legacy copy
cancellation still polls only before chunks, never after completion.

Counters distinguish successfully processed logical chunks from attempted I/O
calls, including failures. Comparison counts an examined unequal chunk before
early exit. Resource arithmetic uses `u128` for requested totals, rejecting
overflow before work. Progress/cancellation boundaries remain outside backend
calls and metadata transactions. Neither completed-byte counts nor cancellation
imply rollback or durability; no flush or publication is introduced.

TDD: five copy/context behavioral tests failed against the initial delegating
implementation, then passed. Two hash/comparison behavioral tests likewise
failed before context integration. Twelve public tests now cover exact final
partial chunks, cumulative reuse, pre-I/O budget refusal, combined scratch,
known SHA-256 content, failed read/write accounting, empty and mismatched sizes,
request overflow, cancellation at initial/intermediate/final boundaries and
legacy callback compatibility. Ten existing image tests also pass in the scoped
run. Red/scoped logs:
`/data/cache/virtdisk-operation-context-red.log`,
`/data/cache/virtdisk-operation-context-read-red.log`,
`/data/cache/virtdisk-operation-context-scoped-final.log`.

Required locked all-feature serial tests pass: 423 passed, zero failed,
68 optional tests ignored across 65 result groups. Log:
`/data/cache/virtdisk-operation-context-tests.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass. Windows target
compilation is not runtime acceptance. Fuzz campaigns and replay remain paused.

See [operation-contexts.md](operation-contexts.md). These APIs are unpublished
development changes after 0.2.0. Native metadata/physical I/O accounting and
context integration with conversion, compaction, checking, recovery, resize,
snapshots/chains and CLI remain open. Reader opening policy consolidation also
remains in the common API stage. The full implementation plan is not complete.

## Context-aware checking and phase progress: 2026-10-08

Added `check_payload_with_context` and `check_image_with_context`, sharing
`OperationContext` and preserving read-only check scope. `OperationPhase`
distinguishes processing, metadata validation and payload validation. Metadata
events have zero logical byte counters because metadata work has no logical
payload measure. A successful `CheckReport` is returned only after requested
validation completes; cancellation at the final payload notification still
returns an error without a success report.

Payload sweeping preflights the complete byte/I/O request before payload reads,
uses fallible bounded scratch and charges failed calls while retaining source
`ReadError` provenance. Metadata constructors and QCOW2 ownership/compressed
descriptor audits retain their separate parser budgets; the context does not
claim to account for that metadata work. QCOW2 periodic cancellation propagates
the typed common cancellation error. Other formats retain non-interruptible
bounded constructors. Existing checking APIs retain cancellation polling points
and share the metadata opener/payload implementation.

Three initial behavioral tests failed against delegation without context
integration. Seven new public tests now pass: quota refusal before payload I/O,
final partial chunks, error provenance and attempted-call accounting,
cancellation before opening/during QCOW2 metadata/between payload chunks/after
the last chunk, empty sweeps, all five families and authorized parent chains.
Every tested check preserves source bytes. Scoped tests also pass seven existing
check tests and twelve operation-context tests. Logs:
`/data/cache/virtdisk-check-context-red.log` and
`/data/cache/virtdisk-check-context-scoped-final.log`.

The current worktree relocated the flake to `nix/`; validation adapted to
`nix develop --no-write-lock-file path:./nix`. The moved lockfile SHA-256 equals
the original tracked lockfile:
`fd297949d676bd92fb446a59f83a58111d5a6d41454c2382d2f64046ca7f2688`.
Required serial locked all-feature tests pass: 430 passed, zero failed,
68 optional ignored across 66 result groups. Log:
`/data/cache/virtdisk-check-context-tests.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass. Windows compilation
does not establish native runtime or servicing correctness. Fuzzing/replay remain
paused. These are unpublished changes after registry 0.2.0.

See [operation-contexts.md](operation-contexts.md). Metadata caller-limit
consolidation, conversion/compaction/native-management context integration,
reader opening policies and CLI integration remain in the common API stage.
The complete implementation plan remains open.

## Immutable reader opening options (development checkpoint)

`ReaderOpenOptions` and `ReadRecoveryPolicy` now select an explicit format,
authorized dependencies, validated parser limits and immutable VHDX log replay
through `Image::open_with_options`. Private fields preserve validation; typed
policies distinguish immutable replay from writer recovery. Existing openers
retain their contracts. Raw deferred reads retain the caller budget, and every
container receives caller limits. Recognition remains separately bounded.

Behavioral regressions verified ignored limits and recovery policy before the
implementation. A tight metadata ceiling also exposed an uncharged QCOW2 fixed
header, which is now charged before reading. A VHDX log with an extended virtual
source exposed incorrect physical inspection size; common image inspection now
reports physical child size while retaining the recovered logical view. Source
bytes remain unchanged. Four reader-options tests and four VHDX recovery tests
pass; one independent QEMU gate remains optional and ignored.

The complete serial locked all-feature suite passes: 435 passed, zero failed,
68 optional ignored across 67 result groups. Log:
`/data/cache/virtdisk-reader-options-tests-complete.log`. Existing QCOW2 mapping
and snapshot validator tests now permit the charged fixed header before testing
refusal during validation; the new one-byte opening regression remains intact.

Formatting and warnings-denied Linux and Windows x64 MSVC all-target/all-feature
Clippy pass. Windows compilation does not establish runtime correctness.
See [reader-opening-policies.md](reader-opening-policies.md). These changes are
unpublished after 0.2.0; fuzzing and replay remain paused. Broader reader-option
chain/descriptor coverage, common management contexts and CLI integration remain
in the common API stage. The complete implementation plan remains open.

## Reader dependency coverage and CLI recovery policy (development checkpoint)

Reader-option tests now cover successful binary recognition and complete
logical inheritance through authorized QCOW2, VHDX, VDI and VMDK parents.
Standalone and empty-authorization openings reject dependencies; caller limits
remain attached to the returned chain, tight metadata ceilings refuse opening,
and both parent and child bytes remain unchanged. Split-flat VMDK descriptors
require explicit format selection and complete extent authorization. Their
cross-extent reads, shared limits and physical extent-set size are verified.

CLI existing-image mutations now use `WriterOpenOptions`: `zero`, `trim`,
`preallocate`, `resize-native`, and QCOW2 snapshot `create|delete|revert` reject
pending recovery by default. Leading `--recover` authorizes supported validated
redo before the requested mutation. Read commands and new-output commands
reject the flag before opening files. Parent authorization is unchanged.
Recovery can change bytes even if the requested mutation subsequently fails;
recovery failure does not imply rollback.

The VHDX CLI recovery regression failed against the old CLI, then passed with
explicit recovery and complete logical-byte verification. Tests verify pending
QCOW2/VDI/VMDK evidence and image bytes are preserved across every applicable
CLI mutation entry point. Six reader-options tests pass. Scoped CLI and VHDX
tests pass; log: `/data/cache/virtdisk-cli-recovery-scoped.log` (the additional
pending-evidence matrix also passed separately). Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

Complete serial locked all-feature tests pass: 440 passed, zero failed,
68 optional ignored across 67 result groups. Log:
`/data/cache/virtdisk-cli-recovery-tests.log`. The run used one build job and
one test thread through the repository's `path:./nix` development shell.

These changes are unpublished after 0.2.0. Windows compilation does not prove
runtime behavior; fuzz campaigns and replay remain paused. Common management
contexts, consolidated metadata accounting, CLI parser budgets and
progress/cancellation remain unfinished. The complete plan remains open.

## CLI input-reader limits and immutable replay (development checkpoint)

Generic CLI readers now use `ReaderOpenOptions` for `info`, `hash`, `map`,
`convert`, `compact`, `compare`, and copy-based `resize`. Leading
`--parser-limit NAME=INTEGER` supports all eight `ParserLimits` fields, rejects
zero/above-default/unknown values before file access, and retains the smallest
value on repetition. Leading `--replay-vhdx-log` selects immutable native replay;
it can be combined with limits in either order. Other commands reject these
controls instead of silently ignoring them. Non-VHDX inputs reject replay.
Options apply separately to each comparison input and persist through deferred
reads and authorized dependencies. Exporters and output validation retain their
own budgets; these controls do not claim a complete management-operation quota.

Behavioral tests failed against the prior CLI before implementation. Coverage
now verifies metadata refusal, deferred raw work exhaustion, successful bounded
hashing, every parser field, monotonic repeated limits, invalid controls before
file access, and clean staging removal after deferred failure in conversion,
compaction and resize. Immutable VHDX replay verifies full logical hashing,
physical EOF inspection despite a larger overlay, and unchanged source bytes.
Scoped CLI/recovery tests pass; log:
`/data/cache/virtdisk-cli-reader-scoped-final.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

See [reader-opening-policies.md](reader-opening-policies.md).
Complete serial locked all-feature tests pass: 446 passed,
zero failed, 68 optional ignored across 67 result groups. Log:
`/data/cache/virtdisk-cli-reader-tests.log`. The run used the repository's
`path:./nix` development shell, one build job and one test thread.

These are unpublished changes after 0.2.0. Fuzzing and replay remain paused.
CLI controls for check and
QCOW2 snapshot operations, common management contexts and progress/cancellation
remain unfinished. Windows cross-compilation is not runtime validation. The
complete implementation plan remains open.

## Checking and snapshot-reader parser limits (development checkpoint)

`check_image_with_limits` and `check_image_with_limits_and_context` accept caller
parser ceilings validated before callbacks or file access. The latter retains
the independent payload `OperationContext` accounting/cancellation contract.
One parser budget covers authorized opening, QCOW2 ownership and compressed
descriptor audits, and deferred payload reads. Existing checking entry points
retain their default limits and cancellation semantics. Generic checked readers
now use the common validated opening path.

CLI `check` and read-only QCOW2 snapshot `list`/`export` accept leading parser
limits. Snapshot limits remain attached to saved views. These commands reject
immutable log-replay controls before file access; limits do not authorize
recovery. Native mutation parser controls remain separate unfinished work.

Four new library tests cover invalid limits before callbacks/file access,
independent parser/payload quotas with attempted-I/O accounting, metadata
ceilings and authorized-parent checks for every container, source preservation,
and typed cancellation before opening. CLI tests cover metadata and payload
refusal without a success report, bounded successful payload sweeping, snapshot
listing and refusal before export creation. The snapshot-control regression
failed against the prior CLI before integration, then passed.

Scoped tests pass: seven existing checks, seven context checks, four new limit
tests and 28 CLI tests; one optional CLI gate is ignored. Log:
`/data/cache/virtdisk-check-limits-scoped.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

Complete serial locked all-feature tests pass: 452 passed, zero failed,
68 optional ignored across 68 result groups. Log:
`/data/cache/virtdisk-check-limits-tests.log`. The run used the repository's
`path:./nix` shell, one build job and one test thread.

These are unpublished changes after 0.2.0. See [check.md](check.md) and
[reader-opening-policies.md](reader-opening-policies.md). Fuzzing and replay
remain paused. Common management contexts, metadata accounting consolidation,
and CLI operation quotas/progress/cancellation remain open. Cross-compilation
does not establish Windows runtime behavior. The complete plan remains open.

## CLI common operation quotas (development checkpoint)

CLI `hash`, `compare`, and `check` now use a common `OperationContext` selected
by leading `--operation-limit bytes=N|io=N|scratch=N` controls. Parser and
operation controls can be interleaved. Quotas are validated before file access;
zero byte/I/O quotas permit empty payload work, scratch is bounded to
1..=131072, and repeated values only tighten limits. Unsupported commands reject
the controls rather than ignoring them. Byte/I/O/scratch accounting remains
independent of input parser budgets and metadata validation.

Hashing/comparison preflight known payload work; comparison counts the logical
range once with two top-level reads per chunk and combined two-buffer scratch.
Structure-only checks accept zero payload byte/I/O quotas while retaining
metadata parser bounds. Quota failures exit with status 2 without success
output; completed unequal comparisons retain status 1.

The initial behavioral quota test failed against the prior CLI. Three new CLI
tests now cover quota refusal for every supported command, successful bounded
hashing, invalid controls before file access, mixed parser/operation controls,
empty hashing with zero quotas, comparison I/O and scratch refusal, monotonic
repetition and structure-only checking with zero quotas. All 31 CLI tests pass;
one optional gate is ignored. Scoped log:
`/data/cache/virtdisk-cli-operation-scoped.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

See [operation-contexts.md](operation-contexts.md).
Complete serial locked all-feature tests pass: 455 passed,
zero failed, 68 optional ignored across 68 result groups. Log:
`/data/cache/virtdisk-cli-operation-tests.log`. The run used `path:./nix`, one
build job and one test thread.

These changes are unpublished after 0.2.0. Management-context integration,
metadata accounting consolidation,
CLI progress/cancellation and native mutation quotas remain open. Fuzz campaigns
and replay remain paused. Cross-compilation does not establish Windows runtime
correctness. The complete implementation plan remains open.

## CLI context progress (development checkpoint)

Leading `--progress` reports synchronous JSON-line context progress on stderr
for `hash`, `compare`, and `check`, preserving stdout results. It combines with
parser and operation limits. Fixed phase names distinguish logical processing,
metadata validation and payload validation. Records include phase byte counts
and cumulative logical/I/O/scratch usage; metadata has zero logical totals.
Other commands reject the flag before file access.

Two initial behavioral progress regressions failed against the prior CLI.
Four new tests now cover final partial chunks and unchanged hash results,
metadata/payload phase separation, quota refusal before payload events,
comparison accounting, empty work, unsupported commands and a closed progress
channel established before spawning. Progress I/O failure stops the read
operation at a context boundary, preserves the I/O error and returns status 2
without a success result or diagnostic panic. Context/observer lifetimes use a
lexical scope; no unsafe code or background threads were introduced.

All 35 CLI tests pass; one optional gate is ignored. Scoped log:
`/data/cache/virtdisk-cli-progress-scoped.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

Complete serial locked all-feature tests pass: 459 passed, zero failed,
68 optional ignored across 68 result groups. Log:
`/data/cache/virtdisk-cli-progress-tests.log`. The run used `path:./nix`, one
build job and one test thread.

See [operation-contexts.md](operation-contexts.md). These are unpublished changes
after 0.2.0. CLI signal cancellation, common management contexts, metadata
accounting consolidation and native mutation quotas remain unfinished. Fuzz
campaigns and replay remain paused. Windows cross-compilation does not prove
runtime behavior. The complete implementation plan remains open.

## CLI structured error output (development checkpoint)

First-position `--json-errors` selects one escaped JSON error record on stderr
for all command failures, including invalid subsequent controls. Plain errors,
successful stdout results and exit statuses remain compatible. Records expose
stable kind/code labels, typed mutation operation/format/range when available,
and common-context quota resource/limit/requested values. Quota integers use
decimal strings to preserve u128 precision. Typed recovery and cancellation
codes are recognized through a diagnostic source walk bounded to 64 entries.
Formatting lives in a private CLI module; no dependencies were added.

The initial quota/error regression failed against the prior CLI. Three new
public CLI tests verify quota and input classification, attempted mutation range
and source preservation, escaped quotes/backslashes/newlines/Unicode, pending
recovery evidence preservation, progress/error coexistence and unchanged
successful hashing output. All 38 CLI tests pass; one optional gate is ignored.
Scoped log: `/data/cache/virtdisk-cli-json-scoped.log`. Independent Python JSON
parsing accepted four error/progress streams, including control characters and
Unicode in source names (`/tmp/virtdisk-json-oracle.py`). Formatting and
warnings-denied Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

See [cli-errors.md](cli-errors.md). These are unpublished changes after 0.2.0.
Complete serial locked all-feature tests pass: 462 passed, zero failed,
68 optional ignored across 68 result groups. Log:
`/data/cache/virtdisk-cli-json-tests.log`. The run used `path:./nix`, one build
job and one test thread.

CLI signal cancellation, common management contexts, metadata accounting
consolidation and native mutation quotas remain open. Fuzz campaigns and replay
remain paused. Cross-compilation does not establish Windows runtime correctness.
The complete implementation plan remains open.

## Structured shared parser-budget errors (development checkpoint)

Shared `ReadBudget` refusal now carries `ParserLimitExceeded`, with private
fields and typed `ParserResource`, effective ceiling and u128 requested usage.
It covers metadata, live cache, work, cumulative decoded output and per-unit
decode buffers. Existing `Unsupported` kinds and display messages are preserved.
Atomic failed charges record observed usage plus the requested amount without
wrapping; counters remain unchanged. Cache reservations retain RAII release.
Deferred-reader `ReadError` provenance preserves the typed quota in its source
chain. Profile-specific constraints outside shared accounting retain their
existing errors; this does not claim complete metadata-limit consolidation.

CLI JSON diagnostics recognize `parser-limit` through wrapped read provenance,
with distinct parser resource names and exact decimal-string limit/requested
fields. The initial CLI classification regression failed before implementation.
Five new library tests verify failure accounting, requests above u64, cache
release after refusal, independent decode-unit/cumulative limits, provenance
and concurrent atomic charges without overspending. The complete CLI suite and
all five parser tests pass; scoped log:
`/data/cache/virtdisk-parser-errors-scoped-final.log`. Formatting and warnings-denied
Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

See [reader-opening-policies.md](reader-opening-policies.md) and
[cli-errors.md](cli-errors.md). These changes are unpublished after 0.2.0.
Complete serial locked all-feature tests pass: 468 passed, zero failed,
68 optional ignored across 69 result groups. Log:
`/data/cache/virtdisk-parser-errors-tests.log`. The run used `path:./nix`, one
build job and one test thread.

Fuzz campaigns and replay remain paused. CLI signal cancellation, common
management contexts, profile-specific accounting and native mutation quotas
remain open. Cross-compilation does not establish Windows runtime behavior.
The complete implementation plan remains open.

## Chain and descriptor parser-bound errors (development checkpoint)

`ParserResource` now includes `RecursionDepth` and `AttributeBytes`. Authorized
QCOW2/VHDX/VDI/VMDK depth checks use one common bound helper. Requested depth
counts images including the child, and the effective ceiling retains smaller
format caps (32 for QCOW2/VHDX/VDI, 64 for VMDK). VMDK hosted/text descriptors
and grain tables also report typed per-object byte limits. These existing checks
retain `InvalidData`; shared-accounting failures retain `Unsupported`. Cycles,
empty required descriptors and structural layout errors retain their validation
paths. Profile bounds do not undo accepted earlier accounting.

CLI JSON errors expose `parser-limit` with `recursion-depth` or `attribute-bytes`
and exact ceiling/requested fields. The authorized-chain type regression failed
against plain errors before implementation. New reader tests cover all four
chain formats and descriptor/table limits (including a valid hosted sparse image
without an embedded descriptor), with unchanged source bytes. A CLI test verifies
the structured labels and preserved kind. Scoped reader, parser and CLI tests
pass; log: `/data/cache/virtdisk-parser-bounds-scoped.log`. Formatting and
warnings-denied Linux and Windows x64 MSVC all-target/all-feature Clippy pass.

See [reader-opening-policies.md](reader-opening-policies.md) and
[cli-errors.md](cli-errors.md). These changes are unpublished after 0.2.0.
CLI signal cancellation, common management contexts, broader metadata accounting
and native mutation quotas remain open. Complete serial locked all-feature tests
pass: 471 passed, zero failed, 68 optional ignored across 69 result groups. Log:
`/data/cache/virtdisk-parser-bounds-tests.log`. The run used `path:./nix`, one
build job and one test thread.

Broader plan work remains open. Fuzz campaigns and replay remain paused.
Windows cross-compilation does not establish runtime behavior. The complete
implementation plan remains open.

## Bounded VDI export streaming (development checkpoint)

As a prerequisite for common conversion/compaction contexts, dynamic VDI export
now uses one fallibly allocated 64 KiB payload/map-serialization buffer instead
of a 1 MiB buffer. Native allocation units remain 1 MiB. The allocation scan
reads every logical chunk, and the payload pass streams only allocated blocks.
The final partial block's padding remains zero through create-new file extension.
Data is still synced before allocation metadata and the header are installed;
no public API or image profile changes.

The bounded-reader regression failed before implementation because the exporter
requested a 1 MiB source read. It now checks chunk boundaries, sparse allocation,
full logical equality and physical final-block zero padding. An injected failure
partway through payload streaming leaves the header/map uninstalled and the
partial destination rejected by the reader. All five ordinary VDI export tests
pass. The existing independent QEMU test was run explicitly: image checking
reported no errors, and conversion to raw matched the source bytes. Formatting
and Linux/Windows x64 MSVC warnings-denied all-target/all-feature Clippy pass.
Windows cross-compilation does not establish runtime persistence behavior.
The complete serial locked all-feature suite passed: 473 passed, zero failed and 68 optional
ignored across 69 result groups. Log:
`/data/cache/virtdisk-vdi-streaming-tests.log`. Checks used `path:./nix`, one
build job and one test thread; no fuzz campaign or replay was run.

This is unpublished work after 0.2.0. Context-aware conversion and compaction
across all supported formats, including export/verification accounting and
cancellation before publication, remain open. Fuzz campaigns and replay remain
paused; the complete implementation plan remains open.

## Bounded VHDX export and metadata streaming (development checkpoint)

Clean dynamic VHDX export and empty native child creation now use one fallibly
allocated 64 KiB streaming buffer instead of a 1 MiB buffer. Native payload
allocation remains 1 MiB, with the existing bounded BAT. Both source passes
read in chunks of at most 64 KiB; allocated payloads are streamed and final-block
padding remains zero through create-new file extension. Metadata tables use the
same buffer; standalone values and retained child values are written at their
native offsets in bounded chunks. Child metadata ownership and profile limits
remain unchanged. Payloads are still synced before BAT/metadata/header installation.
The optional source is matched explicitly, eliminating the payload-path unwrap.

The bounded-reader regression failed before implementation on a 1 MiB read.
It now verifies chunk boundaries, sparse allocation, full logical equality and
physical zero padding. A source failure during payload streaming leaves the
file identifier, headers, BAT and metadata uninstalled. The existing child test
now preserves a patterned optional value larger than 64 KiB and checks parent
immutability. Focused export and differencing tests pass. Independent QEMU
checking reports no errors and conversion back to raw matches source bytes.
Formatting and Linux/Windows x64 MSVC warnings-denied all-target/all-feature
Clippy pass. The complete serial locked all-feature suite passed: 475 passed,
zero failed, 68 optional ignored across 69 result groups. Log:
`/data/cache/virtdisk-vhdx-streaming-tests.log`. Checks used `path:./nix`, one
build job and one test thread. Cross-compilation does not establish Windows
runtime persistence behavior.

This is unpublished work after 0.2.0, preparing common conversion/compaction
contexts for every supported format. Export/verification accounting and
cancellation before publication remain open. Fuzz campaigns and replay remain
paused; the complete implementation plan remains open.

## Conversion and compaction operation contexts (development checkpoint)

`convert_image_with_context` and `compact_image_with_context` now cover all five
supported output families through their existing native exporter profiles.
Default conversion/compaction delegate to those paths. A small private mutable
export-source trait separates synchronous source access from thread-safe disk
readers, so the borrowed observer remains `FnMut` without `Send`/`Sync`, unsafe
code, shared ownership or interior mutability in the production adapter.

Native passes preflight known source work and report `AllocationScan` and
`ImageExport` with exact phase totals. Sparse VDI/VHDX payload totals include
only allocated logical units; QCOW2 sparse export rereads the complete source.
Raw export uses adaptive scratch and counts both logical reads and writes;
zero compaction chunks omit writes. Native exporter file writes, metadata/maps
and syncs remain backend work outside operation I/O accounting. Fixed native
streaming scratch ceilings are documented and refused before callbacks/source
I/O if the caller's limit is too small. Verification counts the logical range
and both attempted readers in `OutputVerification`. Reused contexts retain
successful work and failed attempted calls. Per-pass quotas can refuse later
work after earlier staged work; no output is published in that case.

Materialization still syncs, compares logical bytes and validates QCOW2 mapping
ownership before its no-overwrite hard link. `Publication` is a final zero-byte
cancellation boundary before the hard link. No cancellation is returned after
publication. Staging cleanup remains best effort; post-link directory-sync
failure can leave published output. Sources must remain immutable, and backend
metadata work/individual calls cannot be interrupted internally.

CLI `convert` and `compact` now accept the existing leading operation quotas and
progress controls, retaining structured quota errors and unchanged result/exit
contracts. Known phase names are emitted on stderr. Broken progress output
stops preparation and removes unpublished staging without a success result.

The first materialization test failed because the context APIs/phases were
missing; the CLI test failed on the prior unsupported-controls guard. Behavioral
tests cover every format, dense/sparse accounting, zero units, final chunks,
source errors, reused tight raw quotas, non-thread-safe observers, cancellation
during export and verification and before publication, quota refusal and stage
cleanup. Native QEMU exporter checking/conversion gates passed for QCOW2, VHDX,
VDI and VMDK after the refactor. See [operation-contexts.md](operation-contexts.md).
Scoped context/CLI tests pass; log:
`/data/cache/virtdisk-materialize-context-scoped.log`. Formatting and Linux/Windows
x64 MSVC warnings-denied all-target/all-feature Clippy pass. Complete serial
locked all-feature tests pass: 486 passed, zero failed, 68 optional ignored
across 70 result groups. Log: `/data/cache/virtdisk-materialize-context-tests.log`.
Checks used `path:./nix`, one build job and one test thread. Windows
cross-compilation does not establish runtime persistence behavior.

These are unpublished changes after 0.2.0. Native mutation/recovery and
snapshot/chain contexts, signal cancellation, capability reporting and the
broader plan remain open. Fuzz campaigns and replay remain paused; the complete
implementation plan remains open.

## Iterable opened-handle capability reports (development checkpoint)

`ImageCapabilities::iter()` now enumerates every current native handle operation
without allocation, retaining the existing conservative `get()` classifications.
`ImageOperation::as_str()` centralizes stable operation labels shared by reports
and structured errors. `UnsupportedReason::as_str()` exposes the three existing
refusal categories without changing their public enum or query semantics.

CLI `capabilities read|write IMAGE FORMAT [PARENT...]` reports JSON scoped to the
opened handle, including profile facts, validation level, capacity, parent and
snapshot facts and supported/unsupported operation entries. Read mode uses the
common reader options; write mode explicitly opens a clean writer and retains
its exclusive lock while reporting. Pending evidence is refused and preserved;
`--recover` is rejected. Reader parser/replay controls apply only to read mode,
and operation quota/progress flags are refused before file access. Generic
new-output/graph APIs remain separate from native handle capability entries.

During integration, explicit raw writer options were found to silently ignore
nonempty dependency authorization. A failing regression confirmed that behavior.
`ImageWriter::open_with_options` now rejects those options before file access;
an explicitly empty list remains valid. Legacy opening methods are unchanged.

The API regression initially failed on missing iterator/label methods and the
CLI regression failed on unsupported command syntax. Scoped tests now pass for
all five clean read/write families, all four authorized backed families, resize
restrictions, held locks, preserved pending sidecars, controls and raw options.
A closed stdout pipe returns a structured I/O error, releases the writer lock
and leaves the image unchanged. An independent Python JSON parser checked all
ten clean read/write reports, exact operation labels, booleans, nullable reasons,
profile/access facts and unchanged images. Log for scoped capability, CLI and
writer-opening tests: `/data/cache/virtdisk-capability-reports-scoped.log`.
Formatting and Linux/Windows x64 MSVC warnings-denied all-target/all-feature
Clippy pass. The complete serial locked all-feature suite passed: 493 passed,
zero failed, 68 optional ignored across 71 result groups. Log:
`/data/cache/virtdisk-capability-reports-tests.log`. Checks used `path:./nix`, one
build job and one test thread. Windows cross-compilation does not establish
runtime persistence behavior.

See [capability-reports.md](capability-reports.md). These are unpublished changes
after 0.2.0. Native mutation/recovery and snapshot/chain contexts, signal
cancellation, finer capability restrictions/generic-operation planning and the
broader implementation plan remain open. Fuzz campaigns and replay remain
paused; the complete implementation plan remains open.

## New-output resize operation contexts (development checkpoint)

`resize_image_with_context` now covers every existing output exporter and retains
the explicit shrink policy and immutable flattened-source contract. Default
`resize_image` delegates through a default context. `RequireZero` preflights and
scans the complete removed tail using an adaptive fallible buffer, reports
`TailValidation` relative to that range, and charges complete examined chunks.
A chunk containing nonzero bytes is counted before refusal. Read failures keep
their original I/O kind, count an attempted call and leave incomplete bytes
uncharged. Rejecting the shrink policy invokes no observer or source I/O.

Tail validation, export and verification share cumulative budgets. A later
export quota/profile refusal preserves earlier scan usage. Synthetic zero
growth is logical facade work, including reads that require no underlying
source read. Export/verification and the final publication cancellation boundary
retain the common conversion contract and best-effort staging cleanup.
The CLI `resize` now accepts leading operation quotas and progress controls,
including the stable `tail-validation` phase, without changing policy syntax.

The API regression first failed on missing context entry point/phase; the CLI
regression failed on the prior control guard. Seven behavioral tests now pass
for shrink/growth in all five formats, shared phase totals, tail cancellation
and quota refusal, nonzero tails, explicit data-loss policy, original source I/O
errors, wholly synthetic growth reads and publication cancellation/cleanup in
every format. The scoped CLI/image/initial-resize suite passes; log:
`/data/cache/virtdisk-resize-context-scoped.log`. Independent QEMU info/conversion
and Python JSON parsing checked all ten shrink/grow outputs across the five
formats, including capacity, complete logical content, progress phases and
unchanged sources. Log: `/data/cache/virtdisk-resize-context-oracle.log`.
Formatting and Linux/Windows x64 MSVC warnings-denied all-target/all-feature
Clippy pass. The complete serial locked all-feature suite passed: 501 passed,
zero failed, 68 optional ignored across 72 result groups. Log:
`/data/cache/virtdisk-resize-context-tests.log`. Checks used `path:./nix`, one
build job and one test thread. Windows cross-compilation does not establish
runtime persistence behavior. See [operation-contexts.md](operation-contexts.md).

These are unpublished changes after 0.2.0. Native mutation/recovery and
snapshot/chain contexts, signal cancellation, generic capability planning and
broader plan work remain open. Fuzz campaigns and replay remain paused; the
complete implementation plan remains open.

## Controlled native zeroing (development checkpoint)

Added `zero_image_with_context` with range and cumulative byte/I/O preflight,
64 KiB native zero calls, `Zeroing` progress, cancellation and explicit flush
ownership. Callbacks occur after backend calls return. The common wrapper owns
no scratch; native allocations remain outside context accounting. Failed calls
are charged without marking their bytes completed. Cancellation or per-call
native validation can retain completed prefixes.

CLI operation controls and progress now support `zero`, including explicit
`--recover zero`. A private execution enum preserves ordinary CLI whole-call
validation when controls are absent. Recovery/opening/flush are outside quotas;
recovery may modify an image before a later refusal. Library errors identify the
failed native chunk. Documentation records these failure effects.

TDD first established missing API/phase compile failures and unsupported CLI
controls. Five library tests cover native dispatch, quota/range refusal, lock-free
callback boundaries, empty work, failed attempts and cumulative retry budgets.
The CLI regression exercises all five formats with exact byte verification and
pre-mutation quota refusal.

Validation: formatting, Linux and Windows x64 MSVC all-target/all-feature Clippy
with warnings denied passed. The complete serial host suite passed **507 tests,
0 failed, 68 ignored across 73 result groups**. Log:
`/data/cache/virtdisk-zero-context-tests.log`. Host checks do not establish Windows
runtime or power-loss correctness. Fuzz campaigns and deterministic fuzz replay
remain paused. These changes are unpublished after 0.2.0; the complete
implementation plan remains open.

## External graph operation contexts (development checkpoint)

Added `ImageGraph::{snapshot_with_context, snapshot_as_with_context,
flatten_with_context, rebase_to_with_context, merge_to_with_context}`. Existing
methods delegate with default contexts. Graph authorization, opened identities,
parent edges, depth limits and ancestry checks retain their existing contracts.
Parents, selected states and sibling branches remain immutable.

Native external snapshots in QCOW2, VHDX, VDI and single-file hosted VMDK now
share bounded logical verification, cumulative quotas, progress and cancellation
before publication. New-output rebase uses two fallible scratch buffers adapted
to the caller ceiling; it counts selected/parent reads and native write calls,
skips unchanged writes and parent reads beyond a shorter parent, verifies the
full logical result and offers a final pre-publication cancellation boundary.
Later quota failure retains earlier usage but removes unpublished staging and
registers no graph edge. Backend opening, metadata allocation/validation, native
creation and flush are outside accounting. Post-publication directory-sync or
registration failure can leave a file present without an edge; this limitation
is documented alongside the observer and immutable-source contracts.

TDD established missing context-method compile failures before implementation.
Seven behavioral tests cover all four snapshot families, publication and
verification cancellation, cumulative flatten/merge accounting, invalid ancestry,
zero masking, shorter parents, adaptive scratch, non-`Send` observers, later
quota refusal, staging cleanup and immutable originals. Existing graph and graph
review regressions pass unchanged. The independent QEMU test accepted QCOW2
snapshot/rebase metadata and verified complete byte arrays for those children
and flattened raw, QCOW2, VHDX, VDI and VMDK outputs. Oracle log:
`/data/cache/virtdisk-graph-context-oracle.log`; QEMU version:
`/data/cache/virtdisk-graph-context-qemu-version.log`.

Persistent graph manifests, branch-aware in-place commit/rebase, graph CLI
commands, wider native profiles and native hypervisor/power-loss gates remain
open. Fuzz campaigns and deterministic fuzz replay remain paused. These changes
are unpublished after 0.2.0; the complete implementation plan remains open.

Validation for this checkpoint: formatting and Linux/Windows x64 MSVC
all-target/all-feature Clippy with warnings denied passed. The full serial host
suite passed **514 tests, 0 failed, 69 ignored across 74 result groups**. Log:
`/data/cache/virtdisk-graph-context-tests.log`. The separately invoked ignored
QEMU oracle test passed; it does not establish native VirtualBox, VMware,
Windows/Hyper-V runtime acceptance or actual power-loss correctness.

## Persistent external graph declarations (development checkpoint)

Added `ImageGraph::manifest` and immutable `GraphManifest` values with explicit
`open`, `save`, `images`, `selected` and `open_graph` APIs. Version 1 persists
absolute paths, named format tags, direct parent indices and optional selection,
with a SHA-256 corruption checksum. Regular manifest input is bounded to 1 MiB,
128 nodes, 64 KiB per path and depth 32. Checked framing, fallible buffer
allocation, structural validation and native path codecs preserve existing
reader bounds without introducing serialization dependencies or unsafe code.

Loading parses metadata without opening embedded image paths. Binding requires
exact caller-supplied canonical path authority before live graph opening; extra,
missing and duplicate grants are refused. The normal graph checks then validate
opened identities, aliases and actual native parent edges. Capturing a declaration
also revalidates the original graph and selected registration. Saving uses
synced staged no-overwrite publication. Unicode paths use UTF-8; non-Unicode
Unix names and unpaired Windows surrogates retain platform-native encoding.

TDD established missing type/method compile failures before implementation.
Eight host tests cover independently encoded fixtures, branch/selection restore,
exact authority, permission refusal before ungranted image I/O, native snapshot
families, malformed live children, corruption and truncation, no-overwrite publication,
non-Unicode Unix paths, malformed tags/paths/topology and accepted/refused node and
depth bounds. A Windows-only codec test covers unpaired surrogates and is included in the
cross-target check; native runtime execution remains required.

The manifest is a topology declaration, not an authenticated content checkpoint
or a transaction spanning image and manifest files. Reopening freshly binds file
identities, and raw parents have no native identifiers. Complete dependency
ownership and exclusion of concurrent mutations remain caller obligations.
Saving a new manifest generation is explicit. Absolute-path relocation,
automatic graph/image journaling, external descriptor extent ownership, graph CLI
commands and branch-aware in-place management remain open. See
[graph-manifests.md](graph-manifests.md) for the versioned representation and
failure effects. Fuzz campaigns and deterministic fuzz replay remain paused.
These changes are unpublished after 0.2.0; the complete plan remains open.

Validation for this checkpoint: formatting and Linux/Windows x64 MSVC
all-target/all-feature Clippy with warnings denied passed. The complete serial
host suite passed **522 tests, 0 failed, 69 ignored across 75 result groups**. Log:
`/data/cache/virtdisk-graph-manifest-tests.log`. Windows Clippy also checks the
Windows-only native path codec and test; that is compilation evidence, not
native runtime, hypervisor or actual power-loss evidence. The final lint
adjustment uses typed array chunks inside the Windows-only UTF-16 decoder and
does not change Linux behavior.

## Retained aggregate graph parser budgets (development checkpoint)

Added `ImageGraph::open_with_limits`, `ImageGraph::budget` and
`GraphManifest::open_graph_with_limits`. Explicitly bounded graphs retain one
`ReadBudget` across registration, exact manifest authority binding, identity
revalidation, native ancestor opening, repeated readers and deferred payload
reads. Graph records and supplied/canonical paths consume metadata; canonical
names cannot hide behind shorter symlinks. Existing graph entry points retain their legacy per-reader parser limits and
expose no graph budget. Caller limits are validated before path access.

Private chain-opening helpers accept an existing validated budget instead of
recreating counters, while existing public native opening APIs remain intact.
Snapshot/rebase staged readers share this accounting, including VHDX's final
publication-directory parent resolution. Late quota refusal cleans unpublished
staging without a new graph edge, even after successful difference writes in the
disposable output. Native writer allocation/creation and independent flattened
output validation keep their existing backend/exporter limits. Manifest framing
retains its fixed versioned bounds. These scopes and the distinction from
`OperationContext` are documented in
[graph-manifests.md](graph-manifests.md#cumulative-graph-parser-limits).

TDD established missing graph/manifest budget APIs and demonstrated a short
symlink bypass before canonical-path charging was added. Eight behavioral tests
cover all five reader families, retained counters after graph drop, aggregate
metadata across independent nodes, exact work exhaustion on deferred reads,
typed recursion refusal, invalid-limit precedence, preserved legacy behavior,
staged native validation, late rebase quota refusal and canonical path accounting.
Existing graph, manifest and operation-context regressions pass. The independent
QEMU test now uses an explicitly bounded graph and accepted QCOW2 snapshot/rebase
metadata and exact logical bytes, plus flattened outputs in all five formats.
Oracle log: `/data/cache/virtdisk-graph-limits-oracle.log`.

The complete implementation plan remains open, including atomic image/manifest
generations, in-place branch management, graph CLI commands and broader native
profiles/platform gates. Fuzz campaigns and deterministic fuzz replay remain
paused. These changes are unpublished after 0.2.0.

Validation for this checkpoint: formatting and Linux/Windows x64 MSVC
all-target/all-feature Clippy with warnings denied passed. The complete serial
host suite passed **530 tests, 0 failed, 69 ignored across 76 result groups**.
Log: `/data/cache/virtdisk-graph-limits-tests.log`. The separately invoked QEMU
oracle passed with retained aggregate graph budgets. Host tests and cross-target
Clippy do not establish native hypervisor/runtime or power-loss correctness.

## QCOW2 graph backing interpretation (development checkpoint)

Fixed a graph validation gap for QCOW2 children without a backing-format
extension. A registered raw parent could be a valid QCOW2 container, while the
native chain reader inferred QCOW2 and returned decoded payload instead of the
raw bytes implied by the graph. The graph previously accepted that disagreement
because it checked only an explicitly recorded format string.

A small typed native inspection now reports the resolved backing family, and
graph validation checks it against the registered parent format. Native chain
inference remains unchanged. Explicit `raw` extensions still permit opaque
QCOW2 file bytes; matching inferred raw/QCOW2 declarations still work. Declared
formats and filename authorization do not silently override native interpretation.
The rule applies to ordinary and aggregate-budget graphs, manifest binding and
existing-graph revalidation before reads, snapshot creation or leaf deletion.

TDD reproduced the accepted disagreement and verified that the native reader
returned payload bytes `37` while the raw file began with `QFI` plus its magic
byte. Six behavioral tests cover refusal, both matching detected families,
explicit raw nesting, independently encoded manifest binding, altered live
metadata without mutation/deletion and v2/v3 behavior. The independent QEMU
oracle agrees on complete byte arrays for explicit raw and implicit QCOW2
interpretation in v3 and v2 children, including zero-filled reads beyond the
shorter decoded parent's capacity. Log:
`/data/cache/virtdisk-graph-qcow2-format-oracle.log`.

The broader plan remains open, including atomic graph/image generations,
in-place chain management, graph CLI exposure and remaining format/platform
profiles. Fuzz campaigns and deterministic fuzz replay remain paused. These
changes are unpublished after 0.2.0.

Validation for this checkpoint: formatting and Linux/Windows x64 MSVC
all-target/all-feature Clippy with warnings denied passed. The complete serial
host suite passed **536 tests, 0 failed, 70 ignored across 77 result groups**.
Log: `/data/cache/virtdisk-graph-qcow2-format-tests.log`. The separately invoked
QEMU backing interpretation oracle passed; tool version is recorded in
`/data/cache/virtdisk-graph-qcow2-format-qemu-version.log`. Host tests and
cross-target checks do not establish native runtime or power-loss correctness.

## Atomic snapshot generation publication (2026-10-08)

Added `ImageGraph::snapshot_generation` and its contextual variant. Linux
publishes a fresh directory containing the image and selected graph manifest
through `RENAME_NOREPLACE`, after preparation and file/directory sync in an
owned private sibling directory. An RAII guard removes unpublished staging on
a best-effort basis. Graph capacity is reserved before commit; the new child
is registered immediately after rename and before parent-directory sync.
Other platforms refuse this publication profile before I/O.

Behavioral coverage verifies QCOW2, VHDX, VDI and hosted VMDK generation
reopening at final locations, unchanged source bytes, cancellation at multiple
preparation/publication phases, cleanup, destination races and parent identity
replacement at the commit boundary. Tests first established the missing API
and phase as compilation failures. The final parent sync error path is
specified but not fault-injected; process termination and actual power-loss
validation remain open. No graph CLI command or broader in-place transaction
is claimed by this checkpoint.

Formatting and Linux/Windows x64 MSVC all-target/all-feature Clippy with
warnings denied passed. The serial host suite passed **540 tests, 0 failed,
70 ignored across 78 result groups**. Logs:
`/data/cache/virtdisk-generation-full-tests.log`,
`/data/cache/virtdisk-generation-clippy.log` and
`/data/cache/virtdisk-generation-windows-clippy.log`.
Host tests and cross-target compilation do not establish native Windows
runtime, servicing/capture or power-loss correctness. The broader plan remains
open, fuzz campaigns and replay remain paused, and these changes are unpublished
after 0.2.0.

## Graph snapshot CLI (2026-10-08)

Added `graph snapshot MANIFEST PARENT DIRECTORY FORMAT AUTHORIZED_IMAGE...`
using the atomic generation API. Every existing graph image requires explicit
caller authorization; the input declaration remains unchanged. The selected
child and manifest are published together in a fresh directory. Common progress,
parser limits, payload quotas and JSON errors apply. Recovery and VHDX log
replay are refused before manifest access. Authorization lists are bounded to
128 entries with fallible vector reservation. Success output uses fallible
stdout writes; an output error after publication cannot roll back the directory.

Tests first demonstrated the missing command/control support. New behavioral
tests cover final-location manifest reopening and payload, unchanged input
manifest, explicit authorization, quota refusal with staging cleanup, and
recovery/replay rejection before I/O. The initial complete run exposed an
existing CLI diagnostic compatibility check; retaining `parser` in the replay
refusal message resolved it. Targeted CLI tests then passed, followed by the
complete serial host suite: **543 passed, 0 failed, 70 ignored across 79 result
groups**. Formatting and Linux/Windows x64 MSVC all-target/all-feature Clippy
with warnings denied passed. Logs:
`/data/cache/virtdisk-graph-cli-full-tests.log`,
`/data/cache/virtdisk-graph-cli-targeted.log`,
`/data/cache/virtdisk-graph-cli-clippy.log`,
`/data/cache/virtdisk-graph-cli-windows-clippy.log`.

The previous goal turn made implementation and verification progress through
atomic generation publication; this turn exposes that behavior to CLI callers.
The full implementation plan remains active. Graph inspection/materialization
CLI actions, broader in-place chain transactions and remaining format profiles
and native/power-loss gates are still open. Host tests and cross-target checks
do not establish native Windows runtime or servicing/capture correctness.
Fuzz campaigns and deterministic replay remain paused; changes remain
unpublished after 0.2.0.

## Graph materialization CLI (2026-10-08)

Added graph `flatten`, `merge` and `rebase` commands using existing contextual
library operations. A shared helper bounds authorization to 128 paths with
fallible reservation and opens the manifest under explicit whole-graph authority
and shared parser limits. Flatten and ancestry-checked merge create standalone
outputs; rebase creates a QCOW2 overlay preserving source content over a
registered raw/QCOW2 parent. Existing images and input manifests stay unchanged.
Common progress, JSON errors and operation/parser limits apply. Recovery/replay
are rejected. Fallible success output does not undo published images.

Tests first demonstrated missing materialization command/control support.
Behavioral tests verify complete output payloads for all three commands, source
preservation, rejection of non-ancestor merge targets, and parser/payload quota
refusal without output publication. Existing CLI tests also passed. Formatting,
Linux and Windows x64 MSVC all-target/all-feature Clippy with warnings denied
passed. The serial host suite passed **544 tests, 0 failed, 70 ignored across
79 result groups**. Logs:
`/data/cache/virtdisk-graph-materialize-targeted.log`,
`/data/cache/virtdisk-graph-materialize-full-tests.log`,
`/data/cache/virtdisk-graph-materialize-clippy.log`,
`/data/cache/virtdisk-graph-materialize-windows-clippy.log`.

The previous goal turn made implementation progress through graph snapshot CLI
exposure. This increment extends that CLI with new-output materialization.
Rebase does not publish an updated graph declaration: atomic rebased generations,
persistent branch selection, graph inspection and in-place branch transactions
remain open. These commands do not complete those requirements or the broader
format work. Host/cross-target checks do not establish native runtime,
servicing/capture or actual power-loss correctness. Fuzz campaigns and replay
remain paused, and changes remain unpublished after 0.2.0.

## Atomic rebased generations and post-commit error coverage (2026-10-08)

Added `ImageGraph::rebase_generation` and `_with_context`, plus
`graph rebase-generation MANIFEST SOURCE PARENT DIRECTORY AUTHORIZED_IMAGE...`.
A private enum selects snapshot or rebase preparation under the shared Linux
no-overwrite directory publisher. Rebase preserves registered source content in
a new QCOW2 child over a registered raw/QCOW2 parent and publishes a selected
manifest containing the original graph plus the final child path. Existing
images and declarations remain immutable; no in-place edge change is claimed.

Tests first demonstrated the missing API/CLI. Behavioral coverage verifies
selected final topology, complete logical bytes, unchanged originals and
pre-publication cancellation at export, verification and both publication
boundaries. The non-Linux refusal test now includes rebase generation. A private
parent-sync dependency permits deterministic failure injection after rename:
both snapshot and rebase remain complete, readable and registered when that
sync fails. This closes the prior untested post-commit error contract, without
claiming process-death or actual power-loss coverage.

The serial host suite passed **548 tests, 0 failed, 70 ignored across 79 result
groups**. A Clippy-reported unnecessary test clone was replaced by a borrowed
slice; affected integration tests and the sync-failure unit test were rerun
successfully afterward. Formatting and Linux/Windows x64 MSVC all-target/
all-feature Clippy with warnings denied passed. Logs:
`/data/cache/virtdisk-rebase-generation-full-tests.log`,
`/data/cache/virtdisk-rebase-generation-final-targeted.log`,
`/data/cache/virtdisk-generation-sync-fault-final.log`,
`/data/cache/virtdisk-rebase-generation-clippy.log`,
`/data/cache/virtdisk-rebase-generation-windows-clippy.log`.

The preceding goal turn made implementation progress through materialization
CLI exposure. The broader plan remains active: persistent branch selection,
existing-generation replacement, in-place chain transactions, graph inspection,
remaining format profiles and native/power-loss gates remain open. Host and
cross-target checks do not establish native Windows runtime or servicing/capture
correctness. Fuzz campaigns/replay remain paused; changes are unpublished after
0.2.0.

## Persistent graph selection (2026-10-08)

Added `graph select MANIFEST STATE OUTPUT_MANIFEST AUTHORIZED_IMAGE...` and
`ImageGraph::save_manifest`/`save_manifest_with_context`. A registered disk state
is persisted in a fresh validated declaration without changing existing images,
branches or the input manifest. The contextual API owns metadata-validation and
final publication cancellation boundaries; the CLI uses it rather than exposing
internal phase methods. Payload usage remains unchanged; configured graph parser
budgets retain cumulative accounting. Existing output declarations are refused.
The library can clear selection by passing `None`. Directory sync or stdout
failure after publication can leave the new manifest present.

The CLI test first demonstrated missing selection support. Behavioral tests
cover selected-state reopening and complete payload, unchanged originals,
no-overwrite refusal, unregistered-state rejection, clearing selection and
cancellation cleanup at both boundaries. Existing CLI tests also passed.
Formatting and Linux/Windows x64 MSVC all-target/all-feature Clippy with warnings
denied passed. The complete serial host suite passed **551 tests, 0 failed,
70 ignored across 80 result groups**. Logs:
`/data/cache/virtdisk-graph-selection-targeted.log`,
`/data/cache/virtdisk-graph-selection-full-tests.log`,
`/data/cache/virtdisk-graph-selection-clippy.log`,
`/data/cache/virtdisk-graph-selection-windows-clippy.log`.

Selection is caller-owned manifest versioning, not replacement of an existing
current-state pointer or a hypervisor VM-state revert. Existing-generation
replacement, in-place branch transactions, graph inspection and remaining format
and native/power-loss gates remain open. Host/cross-target checks do not establish
native Windows runtime or servicing/capture correctness. The broader plan remains
active; fuzz campaigns and replay remain paused; changes remain unpublished
after 0.2.0.

## Authorized graph inspection (2026-10-08)

Added `graph info MANIFEST AUTHORIZED_IMAGE...`. Inspection reports selected
and parent indexes, container families, logical sizes and native path diagnostics.
The existing bounded authority helper now accepts a parsed declaration, avoiding
an extra manifest read. All authorized graph readers validate before success
JSON begins. Size collection uses bounded fallible reservation; JSON and native
path encoding stream through fallible writes. Unicode paths are escaped;
non-Unicode paths use null display strings and lossless hexadecimal native
bytes. Unix bytes and Windows UTF-16LE are explicitly labeled. Other targets
label their toolchain-dependent opaque Rust OS-string bytes.

The initial test demonstrated the missing command. Behavioral tests cover
selected state, ancestry indexes and sizes, quoted/newline paths, non-Unicode
Unix filenames, unchanged source bytes, complete authority, parser quota refusal
without success output, and replay rejection before manifest access. Existing
CLI tests passed. Formatting and Linux/Windows x64 MSVC all-target/all-feature
Clippy with warnings denied passed. The complete serial host suite passed
**553 tests, 0 failed, 70 ignored across 80 result groups**. Logs:
`/data/cache/virtdisk-graph-info-targeted.log`,
`/data/cache/virtdisk-graph-info-full-tests.log`,
`/data/cache/virtdisk-graph-info-clippy.log`,
`/data/cache/virtdisk-graph-info-windows-clippy.log`.

Inspection does not infer unknown dependents or confer destructive ownership.
Existing-generation replacement, in-place branch transactions, remaining format
profiles and native/power-loss gates remain open. Cross-target checks do not
establish native Windows path/runtime behavior or servicing/capture correctness.
The broader plan remains active. Fuzz campaigns and replay remain paused;
changes remain unpublished after 0.2.0.

## Contextual owned-leaf deletion (2026-10-08)

Added `ImageGraph::delete_snapshot_with_context` and `SnapshotDeletion`, the
final cancellation boundary before exclusive child opening, identity revalidation
and unlink. `MetadataValidation` precedes graph validation. The existing default
method delegates to the contextual contract. Base/non-leaf refusal happens before
the final boundary; replacement identities after that callback are refused before
unlinking. After successful unlink the graph removes the child before directory
sync, so a later sync error retains the committed graph state. Native metadata,
locking and persistence do not consume payload quota; graph parser accounting
remains cumulative. Concurrent mutation and unknown dependent ownership remain
caller responsibilities.

Tests first demonstrated the missing API and phase. Behavioral coverage verifies
cancellation and unchanged physical bytes for QCOW2, VHDX, VDI and hosted VMDK,
sibling preservation, zero payload-budget usage, base/non-leaf refusal, parser
quota refusal, and replacement at the final boundary. A private sync dependency
permits deterministic post-unlink failure injection; the test verifies removal,
updated declaration and readable surviving branch. No power-loss claim follows
from this fault injection.

Formatting and Linux/Windows x64 MSVC all-target/all-feature Clippy with warnings
denied passed. The complete serial host suite passed **559 tests, 0 failed,
70 ignored across 81 result groups**. Logs:
`/data/cache/virtdisk-graph-delete-targeted.log`,
`/data/cache/virtdisk-graph-delete-full-tests.log`,
`/data/cache/virtdisk-graph-delete-clippy.log`,
`/data/cache/virtdisk-graph-delete-windows-clippy.log`.

Stored declarations are not rewritten by unlink. Atomic persistent graph deletion,
existing-generation replacement, multi-file VMDK deletion/ownership, in-place
branch transactions and remaining format/native/power-loss gates remain open.
No destructive graph CLI action is introduced here. Cross-target checks do not
establish native Windows deletion, runtime or servicing/capture correctness.
The broader implementation plan remains active; fuzz campaigns and replay remain
paused; changes remain unpublished after 0.2.0.

## Authorized backed QCOW2 internal snapshot lifecycle (2026-10-08)

Enabled native internal snapshot creation, deletion and revert over explicitly
authorized immutable raw/QCOW2 backing chains within the existing bounded Linux
writer profile. Native locked-chain validation and journal original/replacement
validation/recovery now receive the writer's parent grants. Capability reporting
reflects the accepted backed profile. Existing geometry, VM-state, active L1,
snapshot directory, physical-size and transaction limits remain unchanged.
Parents are read-only dependencies: snapshots share their immutable content,
without capturing parent changes or changing parent refcounts.

Native CLI create/delete/revert accept trailing authorized parent paths. Existing
pending recovery policy is preserved; CLI recovery still requires `--recover`.
Initial behavioral and CLI tests demonstrated the prior standalone-only and
missing-argument limitations. Tests verify raw and QCOW2 parents, inherited data,
private payload COW, explicit-zero masks, full active and saved logical streams,
revert/deletion, capability support and byte-identical parents. Recovery tests
exercise persistence boundaries and fixture metadata patch cuts for all three
operations over both parent formats; unauthorized reopening preserves child
bytes and journal evidence before authorized recovery.

Independent QEMU ownership checks and full active/saved stream conversions pass
before and after lifecycle operations over both parent formats. Source child
bytes are unchanged by oracle inspection/conversion. Log:
`/data/cache/virtdisk-qcow2-backed-qemu-oracle.log`; tool version:
`/data/cache/virtdisk-qcow2-backed-qemu-version.log`.

Formatting and Linux/Windows x64 MSVC all-target/all-feature Clippy with warnings
denied passed. The serial host suite passed **563 tests, 0 failed, 71 ignored
across 82 result groups**; the new independent oracle was separately invoked.
Logs: `/data/cache/virtdisk-qcow2-backed-full-tests.log`,
`/data/cache/virtdisk-qcow2-backed-targeted.log`,
`/data/cache/virtdisk-qcow2-backed-recovery.log`,
`/data/cache/virtdisk-qcow2-backed-clippy.log`,
`/data/cache/virtdisk-qcow2-backed-windows-clippy.log`.
The last source change after host tests clarified a public documentation comment;
formatting and Windows cross-target compilation checked that final source.

Refcount-table relocation still requires geometry/journal/ownership-bound redesign;
backed/snapshot resize, advanced writable profiles, persistent graph transactions
and remaining format/native/power-loss gates remain open. This work does not
establish native Windows mutation, VM-state capture, servicing/capture or actual
power-loss correctness. The broader plan remains active. Fuzz campaigns and
replay remain paused, and changes remain unpublished after 0.2.0.

## Standalone QCOW2 resize preserves internal snapshots

Native standalone resize now accepts the bounded internal-snapshot lifecycle
profile. Active growth and policy-controlled shrink preserve every saved state's
content and capacity through the existing allocator's copy-on-write mappings and
bounded journal. Capability reporting and CLI tests follow the supported profile.
Backed resize remains unsupported. See [the contract](qcow2-resize.md).

Behavioral coverage includes full/short/empty snapshots, growth, zero-tail refusal,
explicit data-loss shrink, revert/delete after resizing, and a full directory.
Deterministic recovery checks every changed cluster in the growth/shrink fixtures
and persistence cuts, validating complete active/saved bytes and capacities.
QEMU 10.2.4 independently checked ownership and complete active/restored-snapshot
streams for growth and shrink. Restoration uses a copy, leaving source bytes
unchanged; temporary snapshot conversion alone keeps QEMU's active capacity.

Validation: formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy
passed. The full serial host suite passed with **567 passed, 0 failed, 72 ignored**
in 83 result groups; the four resize tests, including the ignored QEMU oracle,
also passed when invoked separately. Logs:
`/data/cache/virtdisk-qcow2-snapshot-resize-full.log`,
`/data/cache/virtdisk-qcow2-snapshot-resize-qemu.log`,
`/data/cache/virtdisk-qcow2-snapshot-resize-qemu-version.log`,
`/data/cache/virtdisk-qcow2-snapshot-resize-clippy.log`,
`/data/cache/virtdisk-qcow2-snapshot-resize-windows-clippy.log`.
The final source-only change after host tests wrapped a documentation comment;
formatting and both Clippy checks checked the final source.

Refcount-table relocation, backed resize, persistent graph transactions and the
remaining format/native/power-loss gates remain open. Host and cross-target checks
do not establish native Windows servicing, capture or actual power-loss
correctness. Fuzz campaigns and replay remain paused. The broader plan remains
active, and these changes remain unpublished.

## Authorized backed QCOW2 resize

The Linux bounded v3 resize transaction now accepts explicitly authorized
immutable raw/QCOW2 parents. Growth masks every new whole cluster to zero and
privately copies an inherited boundary prefix; shrink checks resolved inherited
bytes for `RequireZero`, and preserves retained boundary data while clearing its
hidden suffix. Active ownership, capacity, masks and boundary payload changes
share the existing bounded journal. Saved snapshots retain their bytes and
capacities, parents remain unchanged, and recovery requires parent authorization.
Capability reporting now exposes this supported profile. The existing 64 KiB,
16-bit refcount, sector alignment, 32 GiB virtual/33 GiB physical, single-cluster
active L1/directory, 64-snapshot and 16-cluster transaction limits still apply.
Large growth requiring more metadata patches is refused before mutation.
See [the resize contract](qcow2-resize.md).

Behavioral tests cover both parent formats, nonzero inherited-tail refusal,
mutation-free patch-budget refusal, growth beyond parent capacity, zero-tail
shrink, shrink/regrowth and saved inheritance. Recovery checks every changed
cluster in growth/shrink fixtures and all persistence cuts, unauthorized refusal,
full active/saved streams and unchanged parent files. QEMU independently checked
ownership and full active/restored-snapshot streams for both parent formats and
both resize directions, without changing source files.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed. The
final full serial host run passed: **569 passed, 0 failed, 73 ignored**, 83 result
groups. The six resize tests including both ignored QEMU oracles also passed in
a separate invocation. The first full run exposed a legacy assertion expecting
backed resize refusal; it was replaced with full inherited-prefix/zero-growth and
parent-preservation assertions before the successful final run.
Logs: `/data/cache/virtdisk-qcow2-backed-resize-full-final.log`,
`/data/cache/virtdisk-qcow2-backed-resize-targeted.log`,
`/data/cache/virtdisk-qcow2-backed-resize-recovery.log`,
`/data/cache/virtdisk-qcow2-backed-resize-qemu.log`,
`/data/cache/virtdisk-qcow2-backed-resize-clippy.log`,
`/data/cache/virtdisk-qcow2-backed-resize-windows-clippy.log`.

Refcount-table relocation, advanced QCOW2 profiles, persistent graph transactions
and remaining common/format/platform acceptance gates remain open. Host and
cross-target checks do not establish native Windows servicing/capture or actual
power-loss correctness. Fuzz campaigns and replay remain paused. The broader
implementation plan remains active and changes remain unpublished.

## Common native resize controls

`ImageWriter::resize_with_context` supplies cumulative logical-byte, facade-I/O
and scratch quotas for resolved zero-tail validation, plus a final `NativeResize`
cancellation boundary before native dispatch. Growth and explicit tail loss
consume zero logical bytes and one attempted native call. A verified zero tail
is removed without repeating the scan in the backend. A private dispatch helper
retains ordinary resize behavior and avoids redundant Resize error wrappers.
Backend transactions and flush behavior remain unchanged; metadata/journal work
is outside operation accounting. The CLI accepts native resize controls and
explicit parent paths while preserving whole-call behavior without controls.
See [the contract](operation-contexts.md#native-capacity-changes).

The initial behavioral tests failed to compile because the API/phase were absent.
Coverage now checks raw and every supported Linux container family, bounded
scan accounting, cancellation at the final boundary and mid-scan, cumulative
quota refusal, nonzero-tail refusal, unchanged physical bytes on refusal and
successful retained content. The CLI test verifies authorized backed growth,
missing authorization, zero-call quota refusal, progress serialization and full
inherited/zero-grown bytes with unchanged parent data. Tests use the writable
profiles each backend supports, including dynamic VDI.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed. The
full serial host suite passed: **573 passed, 0 failed, 73 ignored**, 84 result
groups. Logs: `/data/cache/virtdisk-native-resize-context-red.log`,
`/data/cache/virtdisk-native-resize-context-targeted.log`,
`/data/cache/virtdisk-native-resize-context-full.log`,
`/data/cache/virtdisk-native-resize-context-clippy.log`,
`/data/cache/virtdisk-native-resize-context-windows-clippy.log`.
Host/cross-target checks do not establish native Windows servicing, capture or
power-loss correctness. Native backend budgets, persistent graph transactions,
advanced profiles and remaining common/format/platform acceptance gates remain
open. Fuzz campaigns/replay remain paused; the broad plan remains active and
changes remain unpublished.

## Common native snapshot lifecycle controls

`ImageWriter` now exposes context-aware native snapshot creation, deletion and
revert. A small private helper preflights one native facade call and emits the
operation's dedicated zero-byte cancellation phase before dispatch. The existing
methods retain format support, structured errors, locking, authorized parents,
saved capacities/content and journal recovery. No callback runs during or after
mutation; flush remains explicit. Native dispatch counts one call even when it
fails, with zero logical bytes and no facade scratch. Metadata validation,
allocation, journal I/O and synchronization remain outside context accounting.
CLI snapshot create/delete/revert accept operation limits and progress while
preserving whole-call dispatch without controls. See
[the contract](operation-contexts.md#native-disk-snapshot-lifecycle).

Initial behavioral compilation failed because the context APIs/phases were
absent. The implemented coverage proves cancellation and quota refusal preserve
physical container bytes; cumulative create/revert/delete restores saved data;
raw/QCOW2 parents remain unchanged; failed native calls count once; and callbacks
occur once before each attempted operation. CLI coverage verifies quota refusal,
all three serialized progress phases and successful lifecycle commands.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed.
The full serial host suite passed: **576 passed, 0 failed, 73 ignored**, 85 result
groups. Logs: `/data/cache/virtdisk-native-snapshot-context-red.log`,
`/data/cache/virtdisk-native-snapshot-context-targeted.log`,
`/data/cache/virtdisk-native-snapshot-context-full.log`,
`/data/cache/virtdisk-native-snapshot-context-clippy.log`,
`/data/cache/virtdisk-native-snapshot-context-windows-clippy.log`.
This change adds common controls without changing on-disk transaction code;
existing deterministic recovery coverage also passed in the full suite. Host and
cross-target checks do not prove native Windows servicing/capture or actual
power-loss correctness. Native backend budgets, persistent graph transactions,
advanced profiles and remaining common/format/platform gates remain open.
Fuzz campaigns and replay remain paused; the broad plan remains active and
changes remain unpublished.

## Atomic existing graph declaration replacement

Linux `GraphManifest::replace`/`replace_with_context` now replace an existing
manifest while its bytes exactly encode the caller's expected declaration.
The regular singly linked source retains an exclusive nonblocking file lock;
private 0600 sibling staging is written/synced through a retained directory
handle. The final `ManifestReplacement` callback precedes identity/content
rechecks and atomic rename, then parent sync. RAII removes owned unpublished
staging and disarms after publication. Stale declarations, locks, symlinks,
hardlinks, declared-image overlap and late identity/content changes are refused.
Other platforms refuse before I/O/callbacks. Callers serialize management and
exclude noncooperating mutations; expected bytes and inode locks do not lock
arbitrary pathname changes. Metadata/framing remains bounded to 1 MiB and outside
payload quotas. Image content and native parent metadata remain unchanged.

CLI `graph select-in-place` binds exact authorized image paths, validates a
registered selection and replaces the expected prior declaration. Existing
no-overwrite `graph select` remains available. Source image authority and native
log recovery rules are unchanged. See
[the contract](graph-manifests.md#atomic-replacement-of-existing-declarations-on-linux).

Initial behavioral compilation failed because replacement APIs/phases were
absent. Tests prove complete selection replacement, stale-update refusal,
unchanged images, cancellation/staging cleanup, late destination replacement,
locks/aliases, late content changes, CLI authorization/progress and injected
post-rename parent-sync failure. That failure leaves a complete readable
successor and refuses retry with the old expectation. The shared bounded
manifest-file reader was factored for retained-handle validation.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed.
The full serial host suite passed: **581 passed, 0 failed, 73 ignored**, 86 result
groups. Logs: `/data/cache/virtdisk-manifest-replace-red.log`,
`/data/cache/virtdisk-manifest-replace-targeted.log`,
`/data/cache/virtdisk-manifest-replace-full.log`,
`/data/cache/virtdisk-manifest-replace-clippy.log`,
`/data/cache/virtdisk-manifest-replace-windows-clippy.log`.
Actual power-loss and native Windows acceptance are not established by these
checks. Atomic image/manifest deletion, multi-file in-place branch changes,
native backend budgets and other common/format/platform gates remain open.
Fuzz campaigns and replay remain paused; the broader plan remains active and
changes remain unpublished.

## Persistent manifest compatibility and bounded opening

The audit exposed two defects, each confirmed by a failing regression before
correction. A valid v1 manifest can encode a Unicode path using the native Unix
codec; the reader accepts it, but replacement formerly rejected it because the
encoder emits UTF-8. Replacement now compares parsed/normalized declarations
initially, then retains exact original serialized bytes for the final stale
content check. A semantically equivalent encoding change during the final
callback is still refused, preserving the concurrent edit and cleaning staging.
This supersedes the previous ledger's exact-encoder-byte initial precondition.

Linux manifest opening now uses a nonblocking descriptor before bounded regular
file validation. A FIFO without a producer previously blocked the CLI; it now
returns the regular-file refusal promptly. Regular-file framing, checksum,
path/depth authority and allocation limits remain unchanged. The regression
probe has a bounded timeout and explicitly kills/reaps a blocked child on failure.
No equivalent non-Linux FIFO/runtime claim is made.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed.
The full serial host suite passed: **584 passed, 0 failed, 73 ignored**, 86 result
groups, including all three new regressions. Evidence:
`/data/cache/virtdisk-manifest-encoding-red.log`,
`/data/cache/virtdisk-manifest-fifo-red.log`,
`/data/cache/virtdisk-manifest-compat-targeted.log`,
`/data/cache/virtdisk-manifest-compat-full.log`,
`/data/cache/virtdisk-manifest-compat-clippy.log`,
`/data/cache/virtdisk-manifest-compat-windows-clippy.log`.
The targeted log covers the six replacement tests present at that invocation;
the later equivalent-change regression passed in the final full suite.
Persistent image/manifest deletion, multi-file branch transactions, native
backend budgets and remaining common/format/platform gates remain open. Host
and cross-target checks do not prove native Windows servicing/capture or actual
power-loss correctness. Fuzz campaigns/replay remain paused; the broad plan
remains active and changes remain unpublished.

## Common native discard/preallocation controls and persistent deletion design

`ImageWriter::discard_with_context` and `preallocate_with_context` now validate
and preflight the complete range, then emit one final native cancellation phase.
Successful calls account for the requested logical range and one native facade
call; failed native calls charge only the attempted call because partial backend
work is unmeasured. No facade scratch, implicit flush or post-mutation callback
is introduced. Native alignment, strict deallocation/explicit zero fallback,
parent masking, raw preallocation and backend recovery behavior remain unchanged.
Native reads/writes, journal/allocation work and fallback buffers remain outside
payload quotas. CLI trim/preallocate accept controls and preserve whole-call
behavior without them. See [the contract](operation-contexts.md#native-discard-and-preallocation).

Behavioral compilation first failed for absent APIs/phases. Coverage now proves
quota/cancellation refusal preserves physical bytes, partial fallback retains
neighboring data across all supported container families, raw preallocation
preserves content with cumulative quotas, invalid range refusal starts no call,
and backed QCOW2 discard masks active inheritance while preserving parent bytes
and saved inheritance. CLI quota/progress behavior passes for both commands.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed.
The full serial host suite passed: **588 passed, 0 failed, 73 ignored**, 87 result
groups. Logs: `/data/cache/virtdisk-storage-context-red.log`,
`/data/cache/virtdisk-storage-context-targeted.log`,
`/data/cache/virtdisk-storage-context-full.log`,
`/data/cache/virtdisk-storage-context-clippy.log`,
`/data/cache/virtdisk-storage-context-windows-clippy.log`.
These facade changes do not change on-disk transactions; existing deterministic
recovery tests also passed. Host/cross-target checks do not prove native Windows
servicing/capture or actual power-loss correctness.

[Persistent owned-leaf deletion](graph-deletion-transaction-design.md) now has a
concrete proposed journal, authority, publication and recovery-state protocol,
including physical validation budgets and required interruption/foreign-object
acceptance. That operation is not implemented or exposed; a metadata-only
manifest replacement cannot substitute for its image/manifest transaction.
Native budgets, persistent transactions and other common/format/platform gates
remain open. Fuzz campaigns/replay remain paused; the broad plan remains active
and changes remain unpublished.

## Retained-lock manifest publication states

The existing Linux manifest replacement path now uses owned internal
`LockedManifest`, `PreparedManifest` and `PublishedManifest` values. Preparation
consumes and retains the validated old source/directory handles, writes and
syncs private staging, and locks the successor file. Publication consumes
preparation, revalidates exact source/staging bytes and identities, renames once,
and returns an explicit visible-successor state. Parent sync is a separate
fallible method. Both file locks survive through sync. Drop only performs
best-effort identity-checked cleanup of unpublished owned staging; it never
syncs, rolls back or removes a foreign replacement.

This is the retained-lock publication prerequisite for the
[persistent deletion protocol](graph-deletion-transaction-design.md), not an
implemented persistent deletion operation. It avoids reopening/relocking the
manifest between a future journal's steps and permits the coordinator to update
live state after rename before reporting a later directory-sync failure.
Existing public `replace`/`replace_with_context` still publish and explicitly sync
with the same cancellation, authority and post-publication error contract.

Tests first failed for missing owned preparation/publication states. Kernel/file
behavior checks prove the source remains locked while staging is prepared,
abort preserves the source and removes staging, foreign staging is retained on
refusal, and the published successor remains locked until explicit sync. The
existing injected post-rename sync-error and all public replacement regressions
also pass.

Formatting, Linux Clippy and Windows x86_64 MSVC cross-target Clippy passed.
The final full serial host run passed: **591 passed, 0 failed, 73 ignored**,
87 result groups. The first full invocation ended mid-test without a summary;
its handle was unavailable and no Cargo/test process remained, so it was
restarted rather than treated as passed. Authoritative complete evidence is
`/data/cache/virtdisk-manifest-lock-full-retry.log`.
Other logs: `/data/cache/virtdisk-manifest-lock-red.log`,
`/data/cache/virtdisk-manifest-publication-red.log`,
`/data/cache/virtdisk-manifest-lock-targeted.log`,
`/data/cache/virtdisk-manifest-lock-clippy.log`,
`/data/cache/virtdisk-manifest-lock-windows-clippy.log`.
Actual power-loss and native Windows servicing/capture acceptance remain
unestablished. Deletion framing/authority, physical budgets, pending-open guards,
coordinated image/manifest journal mutation, recovery and destructive CLI still
require implementation and acceptance. Other common/format/platform gates
remain open. Fuzz campaigns/replay remain paused; the broad plan remains active
and changes remain unpublished.

## Bounded physical validation prerequisite

Added validated physical byte/read-call/scratch limits, typed quota errors,
cumulative usage and SHA-256 fingerprints. `RawWriter` scans through its retained
locked handle under its mutex. Whole-scan preflight precedes reads/allocation;
failed read attempts consume requested-byte/call budgets while completed hashed
bytes remain separately visible. Limits and exclusions are documented in
[physical validation](physical-validation.md). Existing native journal hashing
has not been migrated; persistent graph deletion journal/recovery/CLI remain open.

Validation: required formatting and Linux all-target/all-feature Clippy passed;
Windows x86_64 MSVC cross-target Clippy passed. The full locked all-feature suite
ran serially with one build job: 595 passed, 0 failed, 73 ignored across 88 result
groups. New cases cover known SHA-256 bytes, cumulative refusal, empty files,
invalid configuration, stale physical length and failed-read accounting.
Complete test evidence: `/data/cache/virtdisk-physical-validation-full.log`;
Windows check: `/data/cache/virtdisk-physical-validation-windows-clippy.log`.
Host and cross-target checks do not establish native Windows runtime correctness
or actual power-loss acceptance. Fuzz campaigns and replay remain paused. The
broad implementation plan pauses here at the user's explicit request to stop
after the current task; changes remain unpublished.
