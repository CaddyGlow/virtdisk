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

## Opt-in implementation experiments

Preparation workers currently apply to raw conversion only; native exporters
remain serial. Larger buffers apply only on paths that honor the request.

The conversion benchmark also accepts `--streaming-allocation`,
`--buffer-kib 64|256|1024|4096`, `--workers 1|2|4`, and `--memory-mib N`.
Use `--formats vdi,vhdx` (or a comma-separated subset of
`raw,qcow2,vdi,vmdk,vhdx`) to focus an experiment on affected exporters.
`--nonzero-run-kib 64|1024` controls sparse clustering: one nonzero run
followed by fifteen zero runs. The default 64 KiB runs place nonzero data in
every 1 MiB VDI/VHDX block, whereas 1 MiB runs leave fifteen of sixteen
blocks unallocated. The CSV identifies the run length.
Supplying any experiment option uses a default 128 MiB working-memory ceiling;
without these options it retains the legacy 128 KiB ceiling and serial defaults.
The CSV records requested streaming policy, buffer size, worker count, and memory
ceiling so incompatible configurations cannot silently merge. Flags request
experiments only on implemented paths; they do not imply that every exporter
uses the requested buffer or worker count.

Streaming allocation uses a single payload pass for VDI and VHDX. It reserves one 1 MiB
logical block plus bounded map/BAT storage, reads each source block once, omits
zero blocks, and appends nonzero blocks with zero padding for the last partial
block. It reuses the block buffer for metadata after payload synchronization.
Insufficient working memory selects the existing two-pass path. Metadata limits
remain 64 MiB per map/BAT. Verification and publication are unchanged. This is
opt-in pending independent scale and native consumer acceptance; host benchmarks
do not establish Windows or power-loss behavior. Sparse QCOW2 uses the same
opt-in control for a bounded allocation bitmap: scanning still reads the complete
source, but the payload pass rereads only nonzero clusters. Its existing refcount
and physical layout algorithm remains unchanged; the bitmap is at most 2 MiB
for a 1 TiB source and is included in operation scratch preflight.

For a diagnostic baseline/candidate comparison, retain separate runs:

```sh
cargo bench --locked --bench conversion -- --size-mib 16 --samples 3
cargo bench --locked --bench conversion -- --size-mib 16 --samples 3 --streaming-allocation
```

Repeat at 256 MiB and larger storage-oriented scales. Compare source scan/read
bytes and calls as well as total elapsed time. Serial run order, shared host load,
and warm caches limit these initial three-round diagnostics; they are not the
independent sessions and confidence evidence needed for a performance claim.

The [retained exporter/pipeline diagnostics](evidence/performance-export-20261010/README.md)
include raw 1/2/4-worker samples and a finite four-buffer smoke with explicit
memory budgets. The report records missing scale, resource, and repeat-session
evidence; those runs do not justify a default change or a speedup claim.
