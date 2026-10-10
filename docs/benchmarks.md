# Performance benchmarks

These benchmarks measure hosted image I/O and conversion. They do not measure
filesystem operations, VM runtime performance, or Windows servicing correctness.
No provider performance ranking is established until comparable runs are retained.

[Windows VM results from 2026-10-10](benchmark-results-windows-20261010.md)
include native VirtualBox and Hyper-V comparisons and the full Rust I/O run.

The [performance improvement plan](performance-plan.md) defines comparison tiers,
profiling work, prioritized optimizations, and correctness and performance gates.

## Rust I/O benchmarks

Run the stable Rust harness in Cargo's optimized bench profile:

```sh
cargo bench --locked --bench io
cargo bench --locked --bench io -- --smoke
cargo bench --locked --bench io -- --write --state fresh --flush 32 --access shuffle
cargo bench --locked --bench io -- --state zero --readers 4 --block 65536
cargo bench --locked --bench io -- --write --state cow --format qcow2 --crossing
```

The original eight CSV columns remain first; configuration, request latency sample
counts, percentiles and cache labels follow. Defaults are 64 MiB images, 4 KiB
requests, 16,384 requests, fixed-stride access, existing allocation, one reader,
and ten independent samples across raw, QCOW2, VDI, VMDK and VHDX. `--smoke`
sets 4 MiB, 64 requests and one sample. There is no unreported warm-up. Every
sample recreates its image so first-allocation and COW samples retain that state.

| Option | Values and meaning |
| --- | --- |
| `--size` | Logical bytes, including multi-GiB and large sparse capacities; format capacity restrictions apply |
| `--block` | Request bytes: e.g. 512, 4096, 65536, 1048576, 4194304; sector multiples up to 4 MiB |
| `--requests`, `--samples`, `--seed` | Workload length, independent sample count and deterministic random seed |
| `--access` | `sequential`, coprime fixed `stride`, seeded Fisher-Yates `shuffle`, `uniform` with replacement, or uniform `hot` access to the first 1/16 of blocks |
| `--state` | `allocated`, `fresh`, `zero`, `fragmented`, or `cow` |
| `--write` | Write instead of read; concurrent writers are rejected |
| `--flush` | `0`: once per pass; `1`: each request; other values: bounded request batch, plus final flush |
| `--readers` | 1, 2, 4 or 8 independent immutable handles; each executes the requested workload |
| `--crossing` | Shift requests by half a request; include crossings of request/mapping boundaries rather than an aligned-only workload |
| `--format` | Restrict to `raw`, `qcow2`, `vdi`, `vmdk`, or `vhdx` |

Shuffled access permutes the first `min(requests, available blocks)` blocks and
repeats that permutation if needed. Sequential and stride access wrap at capacity.
Offset and latency storage scale with requested operations, not image capacity.
`fresh` and `zero` use sparse creation with zero-readable payload; `zero` rejects
writes. `allocated` initializes every logical byte. `fragmented` allocates 1 MiB
logical ranges in alternating low/high order; this disrupts mapping locality
where the format allocator supports it, without promising host fragmentation.
`cow` initializes QCOW2 and retains an internal snapshot before the workload;
it is explicitly excluded outside the supported Linux QCOW2 profile. External
parent-chain COW and format-specific recovery baselines remain future work.
Unsupported native allocation profiles fail with their original error rather
than silently measuring overwrites.

Setup, reader/writer opening, reader/writer close, full logical byte verification and
flush have separately named rows. Writes retain an end-to-end `write_plus_flush`
row and `write_excluding_flush` (elapsed minus measured sync calls). Bounded-batch
and per-request latency includes the applicable flush. Writer handles are dropped
before immutable verification opens; no reader remains alive during mutation.
Payloads change across samples; verification checks touched ranges and every
untouched byte against the expected final state. Read throughput includes reader
thread startup, synchronization and joins, with opening reported separately.

Request p50/p95/p99 are emitted only with at least 1,000 observations, otherwise
`unavailable`; these are individual request timings, not whole-pass quantiles.
The harness reports timer-pair overhead on stderr without subtracting it from
latencies. Sampling itself and thread overhead affect small request measurements.
No cache eviction is performed: `warm` labels this cache-resident-oriented
configuration, not proof that every initial mapping was resident. This is not a
storage bandwidth or host-cold benchmark. Large storage runs need a dataset beyond
both host and guest caches or a documented cache-control method. Select the test
filesystem using `TMPDIR`; retain its machine and storage manifest with CSV.

## External provider comparisons

Build the release CLI, then provide its path (Cargo's configured target directory
may differ from `target`):

```sh
cargo build --release --features cli --locked
python3 scripts/benchmark-providers.py --virtdisk /path/to/release/virtdisk \
  --providers virtdisk qemu --format qcow2 --output qcow2-dense.json
python3 scripts/benchmark-providers.py --virtdisk /path/to/release/virtdisk \
  --providers virtdisk qemu virtualbox --format vdi --pattern sparse \
  --work-dir /path/to/test-filesystem --output vdi-sparse.json
```

On Windows with the Hyper-V PowerShell module installed:

```powershell
python scripts/benchmark-providers.py --virtdisk C:/path/virtdisk.exe --providers virtdisk qemu hyperv --input-format vhdx --format vhdx --output hyperv.json
```

Python 3.11+ and `qemu-img` are required, including for independent verification
of every provider. VirtualBox requires `VBoxManage` and raw input with VDI or VMDK
output. Hyper-V requires native Windows paths, `powershell.exe`, and VHDX input
and output. VHDX input is prepared once with virtdisk, outside timing. Missing
tools, failed commands, unsupported profiles and mismatched bytes fail the run;
they are never recorded as successful samples.

The default source is 256 MiB of seeded pseudorandom dense data. Sparse mode
writes one MiB in every sixteen and leaves zero-filled gaps; physical source-file
hole allocation depends on the host filesystem. Every provider receives
the same immutable input. Each gets one separately labeled warm-up, then ten measured samples in
seeded randomized order. Each destination is fresh. Full independent raw decoding,
length checks and SHA-256 verification happen outside timing after every run.
Reports contain every measured command, elapsed time, logical MiB/s, container
length, host allocated bytes where available, median values, tool versions, host
information, source hash and virtdisk executable hash. Reports never overwrite
existing files. Successful temporary artifacts are removed; failed outputs are
retained beside the report for diagnosis.

Elapsed time includes process startup and the provider's conversion, including
its default flush behavior. `virtdisk convert` includes its built-in staged-output
byte verification and publication within that time; the other commands retain
their own defaults. Hyper-V also includes PowerShell startup and module loading.
The independent verification after each command is excluded for every provider;
it does not equalize the work performed inside the commands.
There is no common durability guarantee, forced
cache eviction or equal sparse-allocation policy across providers. Thus this is
an end-to-end default conversion comparison, not a matched block-backend or
power-loss durability benchmark. Output sizes help expose allocation differences.
Run dense and sparse cases separately, on the same storage and idle host; retain
reports with hardware/filesystem details. JSON preserves raw samples so spread
and outliers can be examined. Small smoke cases validate plumbing, not performance.
Native VirtualBox and Hyper-V runs require their own environments; QEMU validation
does not substitute for those runs.

Provider harness schema 2 selects `default_command_latency` by default.
`--tier matched` selects `matched_application_workflow`: each provider converts to
a private staged file, QEMU independently decodes every logical byte, exact size
and SHA-256 are checked, the container is synced, and a hard link publishes it
without replacing any existing destination. All those wrapper operations are
inside timing. Unsupported hard-link publication fails rather than falling back
to overwrite. On Unix the parent directory is synced after publication; on Windows directory
durability is not established. Virtdisk still performs
its own built-in verification in addition to the common wrapper.

The matched tier records remaining profile differences explicitly: providers
retain their default sparsity, block sizes, metadata layout and flush behavior.
These cases establish a common verified publication workflow, **not** equivalent
allocation or durability. Native consumer acceptance and persistent Hyper-V
library/phase measurements remain explicit exclusions. QEMU decoding checks
logical size and SHA-256 for every warm-up and measured output in both tiers.

The default is now ten paired measured rounds. `--seed` controls deterministic
provider-order randomization; `--session-id` labels independently repeated sessions.
Run at least three independent quiet sessions before drawing performance conclusions.
Reports retain warm-ups, all failures, raw rounds, median/min/max throughput, and
paired bootstrap 95% intervals for virtdisk/provider median-throughput ratios
(at least ten successful paired rounds). These intervals describe one session;
they do not establish repeatability across sessions or remove failed samples from
the run's failure status.

`--timeout` bounds each external command (default 300 seconds). The report is
reserved without overwrite and checkpointed before and after each sample. Command
stdout/stderr and failed output artifacts (`REPORT.diagnostics/`) are retained;
a failed sample makes the process exit unsuccessfully even if other rounds pass.
Interrupted reports remain marked `running`. Artifact retention can require
substantial additional disk space. OS process-tree cleanup and crash-proof atomic
report publication are not established by this harness.

Use `--environment-json manifest.json` to supply filesystem, VM storage chain,
RAM, CPU, host load, and exact release flags. The harness records executable hashes,
commands and source provenance. CPU time, peak RSS and I/O byte counts are explicitly
`unavailable`; physical allocation uses `st_blocks` only on platforms providing it
and is never inferred from container length. Diagnostic tracing belongs in separate
runs. Native VirtualBox/Hyper-V acceptance and Linux/Windows matched reruns remain
required external evidence before claiming a milestone's exit gate.


`--allocation-policy sparse` requests virtdisk's explicit sparse conversion and
QEMU `-S 4k`; VirtualBox/Hyper-V retain their dynamic sparse defaults. These still
have different allocation granularity and layouts, which the profile records.
`--allocation-policy dense` requests QEMU `-S 0` and virtdisk's dense QCOW2 default;
virtdisk dense policy for other outputs and dense VirtualBox/Hyper-V comparisons
are rejected because their policies cannot currently be equalized. `default`
retains original commands. Matched workflow applicability is conditional on
review of these remaining profile differences, and never establishes equivalent
durability merely from choosing the tier.
