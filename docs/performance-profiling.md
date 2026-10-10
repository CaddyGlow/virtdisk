# Diagnostic conversion profiling

Run the stable Cargo entry point with a release build:

```sh
cargo bench --locked --bench conversion -- --smoke
cargo bench --locked --bench conversion -- --size-mib 256 --samples 10
```

The default is 256 MiB and ten samples per case; smoke uses 2 MiB and one sample.
Both sizes and sample counts must be positive. Input creation and cleanup are
outside timing. Each `total` row times the entire public conversion API, including
source scans, payload/metadata output, synchronization, full staged logical-byte
comparison, mapping validation where applicable, and no-overwrite publication.
Inputs use deterministic pseudo-random content, either dense or nonzero in every
sixteenth 64 KiB cluster (6.25 percent). Each format runs its default policy and
explicit sparse policy. Default preserves existing dense QCOW2/raw behavior;
other exporters already omit zero content according to their native policy.

Every case pairs profiling disabled and enabled conversions, alternating the pair
order across samples. The disabled totals show the normal path; enabled totals
include diagnostic clock and mutex overhead. Compare their raw paired totals to
estimate overhead on that workload. These runs are diagnostic warm-cache runs,
not independent sessions or matched provider results, and do not establish a
speedup, a cold-storage result, or native Windows compatibility.

CSV fields identify format, data profile, policy, profiling state, sample, event,
attempted calls, requested bytes, failures, seconds, logical throughput, and
container length. Throughput is provided only for `total`. Container length is
not physical allocation. An operation error makes the harness exit unsuccessfully;
its partial CSV must not be presented as a complete successful matrix.

Enabled rows expose `SourceRead`, `AllocationRead`, `ZeroDetection`,
`PayloadWrite`, `MetadataWrite`,
`Seek`, `Sync`, `OutputReopen`, `ByteVerification`, `MappingValidation`, and
`Publication`. Source-read counters cover exporter payload reads;
`AllocationRead` separately
counts reads used by allocation scans. Neither counts every backend read during
verification or reopening. Byte verification has inclusive timing for its
comparison reads.
Mapping validation includes its QCOW2 reopen. Publication includes the hard link
and, on Unix, the parent directory sync. Native file sync counters do not include
that directory sync. Requested bytes count attempted work and failures remain
visible, rather than proving completed physical I/O.

Event durations are partial components, not an exhaustive exclusive breakdown.
Do not sum inclusive scopes with nested work or interpret an absent event as zero
backend cost. The profiler does not yet instrument cache hits/misses or metadata
allocations, measure OS
read/write bytes or peak RSS, or capture CPU profiles and OS traces. Independent
physical-allocation measurements, sampled CPU profiles and diagnostic OS I/O
traces remain necessary to corroborate these counters. Keep traces separate from
headline timed runs. Full output byte verification remains inside both timed
paths; independent decoders and native consumer acceptance are separate gates.
