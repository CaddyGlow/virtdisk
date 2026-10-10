# Performance improvement plan

Status: M0–M7 implementation delivered; broad performance and release acceptance
gates remain open. Grounded in the
[2026-10-10 Windows VM results](benchmark-results-windows-20261010.md).
Targeted hosted-I/O diagnostics are recorded below; they do not establish a
provider-wide performance lead. The implementation order
is measurement, low-risk I/O improvements, exporter redesign, then concurrency and
transaction optimization. Reorder later work when profiles identify a different
dominant cost.

## Implementation status (2026-10-11)

This is an evidence-gated program, not a claim that every milestone has passed.
The current implementation provides:

- **M0:** default command and timed application-wrapper tiers, explicit allocation
  policy, randomized paired rounds, raw samples and bootstrap intervals, executable
  hashes/operator manifest, timeout diagnostics, failure records, and report
  checkpoints. Unavailable counters and unmatched provider guarantees stay explicit.
- **M1:** opt-in conversion counters and component timings for allocation/source
  reads, zero detection, payload/metadata writes, seeks, syncs, reopening, full byte
  comparison, QCOW2 mapping validation, and publication. A separate paired bench
  exposes instrumentation overhead. Cache counters and OS/profile corroboration
  remain pending. See [profiling usage and limits](performance-profiling.md).
- **M2:** configurable request/access/state/durability/concurrency workloads, named
  lifetime/flush/verification measurements, full expected-state verification, and
  request percentiles with sample counts. See [benchmark usage](benchmarks.md) for
  supported states and remaining parent-COW/scale acceptance work.
- **M3:** explicit API and CLI sparse policy retaining existing defaults, exact
  output verification and no-overwrite publication. Regression coverage includes
  density/run-length/partial-cluster cases, cancellation, and compact 6.25-percent
  QCOW2 payload allocation.
- **M8:** evidence requirements and proposed gates below remain the policy.
  Dedicated multi-session enforcement and native acceptance are still required.

- **M4:** Unix positioned immutable reads, serialized positioned raw writes,
  contiguous allocated VMDK grain reads, and private export cursor tracking remove
  redundant seeks. Windows retains the seek/mutex fallback. Explicit 64/256 KiB
  and 1/4 MiB buffer choices remain bounded by operation memory. See
  [hosted read evidence](performance-hosted-reads.md).
- **M5:** opt-in single-pass VDI/VHDX allocation and a bounded sparse QCOW2 bitmap
  reduce source rereads. Low-memory callers retain the old algorithms. Full
  staged comparison, sync ordering and no-overwrite publication remain mandatory.
  Counter tests, partial tails, independent QEMU decoding and injected payload,
  metadata and sync failures cover the new paths.
- **M6:** opt-in 2/4-worker raw preparation uses one reusable buffer per worker,
  bounded channels and one write coordinator. Worker, coordinator, cancellation
  and panic tests require joined workers and unpublished output on failure.
  Native output and verification remain serial; one worker remains the default.
  [Worker samples and four-buffer smoke results](evidence/performance-export-20261010/README.md)
  retain the limits of the current timing evidence.
- **M7:** the lifecycle benchmark covers first allocation, partial updates,
  supported QCOW2 snapshot COW, discard, resize, compact, reopen and clean
  recovery-policy opening. Unix raw positioned writes preserve the existing
  operation mutex, locks and flush contract. See [lifecycle evidence and limits](performance-lifecycle.md). No new transaction-batching or delayed durability
  contract was introduced.

Linux diagnostics validate benchmark plumbing and independent QEMU decoding;
tmpfs results do not establish storage bandwidth. Targeted syscall counts and
paired raw-read/write measurements support the narrow I/O changes. Streaming
allocation and worker/buffer controls remain explicit experiments, not changed
performance defaults. The original Windows provider results remain baseline
evidence. Quiet three-session matched provider runs, multi-GiB storage tests,
broader native consumer coverage and consumer integration traces are release gates,
not implied by completing the implementation. Focused dense/sparse streaming VDI
VirtualBox decoding and VHDX Windows read-only attachment passed exact whole-image
hash checks; see [native Windows evidence](performance-windows.md).
M0–M7 exit criteria below are
retained so incomplete evidence remains visible.

## Objective and boundaries

Make virtdisk competitive with, then faster than, the fastest applicable QEMU,
VirtualBox, and Hyper-V workflow for explicitly supported image profiles. Publish
the workloads where we lead and those where we do not. A universal claim across
all formats, machines, and durability policies is not a meaningful release gate.

Keep bounded memory, immutable `ReadAt` readers, exclusive writer locks, validated
metadata, recovery ordering, cancellation, and verified no-overwrite publication.
Faster conversion must retain full output byte verification. Do not gain a timing
advantage by removing syncs, ignoring failures, or moving required work outside
the measured operation.

This plan covers image conversion, reading, writing, allocation, COW, and offline
operations. Filesystem creation, browsing, and expansion belong to consumers such
as partmgr; their integration workloads are useful acceptance cases, but adding
filesystem implementations is outside this performance project.

## Starting evidence

The following are median logical MiB/s for 256 MiB conversions. The ratio is
fastest observed competitor throughput divided by virtdisk throughput; it is a
description of this run, not the speedup required under future matched settings.

| Output | Data | virtdisk | Fastest observed competitor | Ratio |
| --- | --- | ---: | ---: | ---: |
| QCOW2 | Dense | 531.2 | QEMU 1365.1 | 2.57× |
| QCOW2 | Sparse | 511.5 | QEMU 1821.2 | 3.56× |
| VDI | Dense | 361.2 | QEMU 926.2 | 2.56× |
| VDI | Sparse | 1059.5 | QEMU 2397.5 | 2.26× |
| VMDK | Dense | 467.5 | VirtualBox 525.0 | 1.12× |
| VMDK | Sparse | 1426.8 | VirtualBox 2191.8 | 1.54× |
| VHDX | Dense | 439.1 | QEMU 980.4 | 2.23× |
| VHDX | Sparse | 1611.5 | QEMU 2208.3 | 1.37× |

Important qualifications:

- virtdisk conversion includes staged-output verification and publication. Peer
  default commands do not establish equivalent guarantees.
- Sparse QCOW2 output was 256.31 MiB for virtdisk and 16.31 MiB for QEMU.
  virtdisk already has a sparse QCOW2 exporter, but normal conversion selects the
  dense path. Investigate policy before treating this as a slow sparse algorithm.
- Sparse VHDX output was 20 MiB for virtdisk, 136 MiB for QEMU, and 260 MiB for
  Hyper-V. These are container lengths, not measured physical allocation.
- Hyper-V measured roughly 108–111 MiB/s including a fresh PowerShell process and
  module loading. This does not establish that its conversion engine is slower.
- The VM had 6 GiB RAM; 256 MiB conversions and 64 MiB Rust I/O cases fit in cache.
  The host was shared and busy. These results cannot establish storage bandwidth.
- Rust benchmarks cover existing-allocation overwrites with one flush per pass.
  They do not measure first allocation, COW, or durable per-request latency.

The Rust baseline also identifies useful internal comparisons: QCOW2 sequential
reads were 3540.8 MiB/s versus raw at 5736.9; VHDX sequential writes were 769.7
versus raw at 1169.5; VDI 4 KiB reads were 713.9 versus raw at 908.8. These suggest
investigations, not proven explanations of overhead.

## M0 — Establish trustworthy comparison tiers

**Priority:** P0. **Dependencies:** none.
**Files:** `scripts/benchmark-providers.py`, `benches/io.rs`, benchmark docs.

Maintain three separate result sets:

1. **Default command latency:** preserve the existing benchmark to represent what
   users experience, including startup and each tool's default guarantees.
2. **Matched application workflow:** align input/output profiles, sparsity policy,
   block sizes where configurable, verification, and publication requirements.
   Put equivalent wrapper work inside timing where feasible. Record differences
   that cannot be equalized; do not label those runs durability-equivalent.
3. **Library and phase costs:** measure virtdisk components, and separately measure
   Hyper-V work in an already initialized PowerShell session. Keep startup-inclusive
   results alongside these figures. Do not compare a library loop with a fresh
   external process as if they were the same operation.

Extend the harness with:

- Explicit profiles, exact commands, input provenance, release build flags,
  executable hashes, OS/filesystem, VM storage chain, RAM, CPU, and host load.
- Timeouts, per-sample checkpoints, failure records, and retained diagnostics.
  Failed or unverifiable samples must never disappear into a successful average.
- Elapsed time, logical throughput, CPU time, peak RSS, container length, physical
  allocation where measurable, and bytes read/written where measurable. Use
  `unavailable` for unsupported measurements; never infer allocation from length.
- Randomized paired rounds, raw samples, medians, spread, and confidence intervals
  for throughput ratios. Start with at least ten measured rounds per target case
  and repeat on three independent sessions; investigate wide intervals before
  making a winner claim. Separate diagnostic tracing from headline timed runs.
- Independent output decoding and exact size/hash checks for every sample.
  Include QEMU plus native VirtualBox/Hyper-V acceptance for applicable formats;
  one decoder is not a substitute for all native consumers.

**Exit:** rerun the original matrix on a quiet Windows VM and Linux environment,
retain machine-readable evidence, and identify which cases genuinely support a
matched comparison. Explain every excluded provider/profile explicitly.

## M1 — Measure the work inside each operation

**Priority:** P0. **Dependencies:** M0 result schema.
**Files:** `src/image.rs`, `src/operation_context.rs`, `src/export_source.rs`,
`src/source.rs`, and format exporters.

Instrument allocation scan, source reads, zero detection, payload writes,
metadata writes, syncs, output reopening, byte verification, mapping validation,
and publication. Existing operation phases are a starting point; native writes,
metadata work, and syncs need visibility beyond source-read accounting.

Count calls and bytes, cache hits/misses, seek operations, metadata allocations,
and verification passes. Distinguish nested timings from exclusive phase costs
so totals do not double-count work. Instrumentation must be opt-in and its overhead
measured. Use sampled CPU profiles and OS I/O traces to corroborate counters.

Investigate these concrete code observations:

| Area | Existing behavior | Hypothesis to test |
| --- | --- | --- |
| `source.rs` | File reads take a mutex, seek, then read | Position-independent reads may reduce calls and contention |
| `qcow2_write.rs` | Dense allocation by default; 64 KiB payload writes; sparse export scans first | Allocation policy, call count, and extra reads may dominate |
| `vdi_write.rs`, `vhdx_write.rs` | Allocation scan followed by payload pass with a 64 KiB buffer | Dense inputs pay avoidable scan/read overhead |
| `vmdk_write.rs` | Grain writes interleaved with table placement and sync ordering | Larger payload batches may help without changing the format |
| `image.rs` | Full staged-output comparison, then QCOW2 mapping validation | Verification and reopen costs need separate attribution |
| Format readers | Reads split at mapping boundaries | Physically contiguous runs may be coalesced |
| `raw_write.rs` | Serialized seek/read/write/resize/flush state | Optimize I/O calls while retaining writer ordering |

**Exit:** a cost breakdown for every target format on dense and sparse inputs,
including small cached and large streaming cases. Pick the next change using
measured dominant costs, not aggregate rankings alone.

## M2 — Expand the classic Rust benchmark suite

**Priority:** P0/P1. **Dependencies:** M0 schema; can proceed alongside M1.
**Files:** `benches/io.rs`, new focused benches as needed.

Keep the stable Cargo benchmark entry point and CSV output. Add parameterized
image size, request size, seed, allocation state, and flush cadence. Keep setup,
opening, steady-state operations, flush, and verification as separately named
measurements; retain an end-to-end write-plus-flush measurement as well.

| Dimension | Required cases |
| --- | --- |
| Request size | 512 B, 4 KiB, 64 KiB, 1 MiB, 4 MiB; aligned and boundary-crossing |
| Access | Sequential, current fixed-stride permutation, seeded shuffled permutation, uniform random with replacement, hot-set access |
| State | Existing allocation, fresh allocation, zero/unallocated reads, fragmented mappings, supported parent/COW paths |
| Durability | Flush per pass, bounded batch, and per operation; report separately |
| Lifetime | Open/validate/close, repeated small operations, sustained streaming |
| Concurrency | One reader, then 2/4/8 readers where supported; no assumed parallel writer contract |
| Scale | Small cached images, multi-GiB images, metadata-heavy large sparse images |

Record latency percentiles for sufficiently long request workloads, with sample
counts and timing overhead. Do not infer p99 from seven whole-image samples.
Change write payloads across iterations and verify the expected final state.
Add focused measurements for mapping lookups, zero scans, checksums, and bounded
cache behavior only when M1 identifies them as material costs.

For storage tests, use a dataset exceeding relevant guest and host caches or a
documented cache-control method. Dropping only guest caches does not establish a
cold host disk. Label warm, guest-cold, and storage-oriented runs accurately.

**Exit:** reproducible allocation/COW/durability baselines in addition to the
existing overwrite tests, with no reader/writer lifetime contract violations.

## M3 — Fix sparse-policy parity first

**Priority:** P1. **Dependencies:** M0 and M1.
**Files:** `src/image.rs`, `src/qcow2_write.rs`, CLI and provider harness.

Expose or route an explicit sparse conversion policy through the existing sparse
QCOW2 exporter. Preserve dense behavior compatibility until the API/default-policy
decision is documented. Benchmark both policies, not a silent default switch.

Test all-zero, dense random, alternating clusters, partial final clusters, and
1/6.25/25/50/100 percent nonzero data. Vary run lengths and clustering. Distinguish
logical zero content from holes in the source container: an allocated container
extent is not necessarily nonzero guest data, and backing images change meaning.

**Exit:** the original 6.25-percent payload case produces approximately payload
plus QCOW2 metadata rather than a fully allocated 256 MiB image, independently
decodes correctly, and retains full verification/publication. Report scan cost
and physical allocation alongside speed. Do not claim a speedup before measuring.

## M4 — Reduce I/O calls and mapping overhead

**Priority:** P1. **Dependencies:** M1/M2 profiles.
**Files:** `src/source.rs`, format readers/exporters, `src/raw_write.rs`.

1. Prototype platform-specific positioned file reads behind the hosted source
   abstraction, with a portable fallback. Preserve exact-read behavior, overflow
   checks, short-I/O handling, identity, and error classification. Verify Windows
   semantics explicitly before removing any seek mutex.
2. Sweep 64 KiB, 256 KiB, 1 MiB, and 4 MiB export/verification buffers under the
   operation memory budget. Reuse buffers; avoid allocations per request.
3. Coalesce adjacent guest mappings only when physical continuity and identical
   zero/backing/compression semantics are proven. Stop at protected boundaries.
4. Batch contiguous payload and table writes; avoid redundant seeks where the
   sequential position is already known. Retain payload-before-metadata syncs.
5. Measure metadata-cache contention and memory growth before changing cache
   layout or eviction. Preserve cache reservations and configured limits.

Keep changes independently reviewable. First optimize immutable reads and private
export files; in-place writers have stronger ordering requirements. Positioned
I/O alone does not authorize concurrent resize, write, or flush.

**Exit:** targeted call counts and elapsed costs fall on matched workloads without
material regressions on small/random cases or exceeding memory limits. Reject
buffer growth whose benefit disappears outside warm-cache microbenchmarks.

## M5 — Reduce exporter passes without weakening verification

**Priority:** P1/P2. **Dependencies:** M3/M4 and phase evidence.
**Files:** `src/vdi_write.rs`, `src/vhdx_write.rs`, `src/qcow2_write.rs`,
`src/vmdk_write.rs`, `src/image.rs`, operation-context accounting.

Prototype streaming allocation for VDI/VHDX: reserve bounded metadata, inspect a
logical block, append nonzero payload once, and install the final map/header after
payload durability. Compare one-block buffering with the current two-pass
algorithm. The additional memory must be reserved and reflected in preflight;
low-memory callers need a bounded fallback.

QCOW2 needs a separate layout design because refcount coverage and final physical
placement depend on allocation. Evaluate a bounded allocation bitmap or temporary
index versus an incremental layout. Account for index I/O, cleanup, metadata
limits, refcount growth, and malformed-source handling. Do not reuse a VDI layout
strategy without proving QCOW2 invariants.

Retain full staged-image byte comparison against the immutable source. Improve its
read size and mapping traversal, and avoid redundant opens only where validation
and identity guarantees remain equivalent. Replacing exact comparison with a
digest is a separate semantic proposal, not an automatic pass-elimination trick.

Update progress, cancellation, and I/O-budget accounting for new algorithms.
Partial final blocks must remain zero-padded. Test failures between payload,
metadata, sync, verification, and publication, including disk-full failures.

**Exit:** fewer measured source bytes/calls, independently valid output, unchanged
publication guarantees, and documented memory/temporary-space bounds. Keep the
old algorithm available until the replacement passes correctness and scale gates.

## M6 — Add bounded parallel work where profiles justify it

**Priority:** P2. **Dependencies:** M4/M5 stable sequential paths.

Try a small pipeline for source reads, zero detection, and payload preparation;
then evaluate independent output ranges on private staged images. Start with
1/2/4 workers and a bounded queue. Measure throughput, CPU, RSS, and cancellation
latency; a default worker count must follow evidence across storage types.

One coordinator owns layout, metadata ordering, sync, verification completion,
and publication. Prove disjoint payload ranges before concurrent writes. Bound
all in-flight buffers in the operation budget. On failure, stop workers, join
them, preserve the original error, and clean unpublished staging state.

Do not introduce parallel mutable image access or weaken exclusive locking as
part of this change. Keep serial execution for constrained callers and workloads
where concurrency increases seek traffic or tail latency.

**Exit:** repeatable end-to-end improvement beyond the sequential implementation,
with bounded resources and fault/cancellation tests covering worker failures.

## M7 — Optimize real write and lifecycle workloads

**Priority:** P2. **Dependencies:** M2 and format-specific recovery baselines.
**Files:** `src/*_writer.rs`, allocation/journal/sidecar modules and integration tests.

Measure first-write allocation, partial-block updates, parent COW, discard,
resize, compact, and recovery separately for supported profiles. Capture payload,
metadata, journal, and sync amplification per operation. Include clean reopen and
recovery time so deferred work cannot hide behind a fast foreground result.

Investigate metadata reuse, bounded allocation batches, and transaction batching
only where the existing contract permits it. Any new batch API must specify when
data becomes durable, partial-failure effects, cancellation, and recovery behavior.
Never silently replace per-operation durability with delayed flushes.

Use partmgr integration traces for partition creation, filesystem formatting,
file copy/readback, and supported volume growth. Time the consumer and image
layers separately. Keep unsupported filesystem/profile combinations explicit.
Host unit tests alone cannot establish native Windows integration correctness.

**Exit:** each claimed write improvement passes format-specific crash/recovery,
locking, reopen, and independent/native-reader acceptance. Publish p95/p99 and
amplification as well as throughput.

## M8 — Set release gates and retain evidence

**Priority:** P0 for evidence policy; P2 for performance enforcement.
**Dependencies:** stable M0 baselines; each optimization uses this gate.

Initial engineering targets, to ratify after the matched baseline:

| Goal | Proposed acceptance rule |
| --- | --- |
| Correctness | Every sample verifies; required regression/recovery/native acceptance passes |
| Competitive conversion | At least 90% of the fastest applicable matched provider's throughput in each designated primary case |
| Demonstrated lead | At least 110% of the fastest applicable matched provider; lower 95% confidence bound of the throughput ratio above 1.0 across independent sessions |
| Internal I/O | Close at least half the gap to same-run raw throughput in targeted allocated sequential workloads, when raw is faster |
| Regression control | Investigate a repeatable >5% throughput loss or >10% tail-latency increase in a designated case before accepting |
| Resources | Stay within documented memory/I/O limits; explain CPU, RSS, allocation, and temporary-space tradeoffs |

Choose the primary matrix before seeing optimized results: all four container
outputs, dense and 6.25-percent nonzero sources, Windows and Linux where providers
apply, small startup-sensitive and large streaming cases. Retain broader density,
fragmentation, and request-size sweeps as secondary results. Unsupported peer
profiles are exclusions, not wins. Report every primary case; do not average away
a format regression or use Hyper-V startup as evidence of engine superiority.

Use dedicated performance runs for enforcement. Shared CI runs smoke correctness
and benchmark build checks, because noisy timing thresholds create false alarms.
For code changes run the repository-required checks:

```sh
cargo fmt -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
```

Also run affected feature/platform checks and native acceptance for the modified
paths. These commands alone do not establish Windows behavior or power-loss safety.

Each optimization report contains the commit/build identity, baseline and candidate
raw samples, machine/tool manifest, exact commands, profiles, correctness evidence,
phase-cost changes, confidence intervals, resource costs, and remaining losses.
Keep one optimization per experiment until its effect is understood. Revert or
disable experiments that fail the gate; do not accumulate speculative complexity.

## Delivery sequence

1. **Measurement PR:** M0, M1, and the first M2 extensions; rerun the baseline.
2. **Sparse QCOW2 PR:** explicit policy and matched allocation comparison (M3).
3. **Hosted read PR:** positioned reads if supported by profiles and platform tests.
4. **Batching PRs:** buffer sweeps, contiguous read/write batching, one format at a time.
5. **Exporter PRs:** VDI/VHDX pass reduction first if profiles confirm the cost;
   QCOW2 layout redesign separately (M5).
6. **Pipeline PR:** only after serial throughput and resource costs are understood.
7. **Write-path PRs:** allocation/COW/transaction changes with individual recovery gates.
8. **Release evidence PR:** complete matched provider rerun and explicit per-case standings.

The implementation sequence above has produced the M0–M7 changes described in
the status section. Further default changes and provider competitiveness claims
remain conditional on the retained evidence and recovery gates; implemented
experiments alone do not establish a speedup.
