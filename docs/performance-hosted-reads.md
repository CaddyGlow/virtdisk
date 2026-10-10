# Hosted read and VMDK mapping experiment (2026-10-10)

This implementation removes Unix cursor seeks from immutable hosted reads and
coalesces proven contiguous allocated VMDK grains. It does not establish a release
performance claim. Windows and other non-Unix systems retain their serialized
seek/read fallback. Retained handles, storage identity and fixed logical lengths
remain unchanged; writer ordering and exclusive locks are not relaxed.

The positioned exact-read loop retries interrupted calls, advances after short
reads, reports early EOF, and rejects range overflow before issuing I/O. Tests
cover those cases, a shared retained handle with eight concurrent readers, unchanged
cloned-file cursor, truncation and retained length after writer growth.

VMDK coalescing uses the already validated immutable grain map. A run ends at
noncontiguous physical storage, an unallocated/inherited grain, an explicit zero
grain, or a descriptor extent boundary. It does not allocate extra buffers and
still charges a work item for every logical grain traversed. A partial-first and
partial-last grain fixture gives these source-call counts:

| Mapping | Before | After |
| --- | ---: | ---: |
| Four consecutive allocated grains | 4 | 1 |
| Allocated, inherited zero, allocated, explicit zero | 2 | 2 |
| Four physically discontinuous allocated grains | 4 | 4 |

These fixture counts measure library source calls. A separate Linux `strace -f`
run of 1,000 raw 4 KiB reads plus setup and full output verification recorded:

| Selected syscall | Before | After |
| --- | ---: | ---: |
| `read` | 1010 | 6 |
| `lseek` | 1010 | 2 |
| `pread64` | 0 | 1004 |
| Total | 2020 | 1012 |

Tracing is diagnostic and excluded from throughput comparisons. Small loader and
setup calls are included in these syscall totals. The runs used the same benchmark
arguments and immutable dataset construction.

The archived baseline is commit `1e0b180`, extracted into
`/tmp/virtdisk-m4-baseline`. Candidate builds contain concurrent implementation
work; the retained manifest hashes the relevant reader source files and benchmark
executables. Both use the repository Nix development environment and optimized
Cargo bench profile with the user's Cargo configuration intact.

```sh
cargo bench --locked --bench io -- --format raw --block 4096 --requests 16384 --samples 10
cargo bench --locked --bench io -- --format vmdk --block 1048576 --requests 64 --samples 10 --access sequential
strace -f -c -e lseek,pread64,read -o syscall-counts.txt /path/to/io --smoke --format raw --requests 1000
```

All measured samples perform full logical byte verification. They are warm-cache
workloads on a shared Linux host, with no cache eviction. The host was concurrently
building and running other experiments. Whole-pass raw samples contain large
scheduling gaps despite sub-microsecond/low-microsecond request medians; these runs
cannot establish either a speedup or a repeatable regression. A quiet-host paired
rerun across independent sessions is still required for the plan's throughput and
tail-latency gates. No storage-bandwidth, Windows positioned-I/O, or native
VirtualBox acceptance claim follows from this experiment.

A subsequent finite diagnostic removed single-reader thread startup from both
benchmark executables and alternated baseline/candidate order across ten pairs.
Each run constructed and fully verified a 16 MiB tmpfs image. Raw used 8,192
4 KiB stride reads; VMDK used 64 sequential 1 MiB reads. Median paired throughput
ratios were 1.185 for raw (95% paired bootstrap median interval 1.069–1.555)
and 1.080 for VMDK (0.974–1.896). These are one-session warm-tmpfs diagnostics,
with substantial per-pair variation on the shared host; they do not satisfy the
independent-session acceptance gate. VMDK has too few requests to report p99.
All forty runs and their order, commands, executable hashes, host load and summary
are retained in [the paired diagnostic evidence](evidence/hosted-reads-20261011/summary.json).
Original outputs remain in `/data/cache/virtdisk-m4-direct-paired/`; earlier noisy
runs remain available unchanged.

Raw CSV, syscall traces, call-count test logs and machine/source manifest are
retained under `/data/cache/virtdisk-m4-*`. Standard and no-default-feature
portable reader tests passed. The four QEMU VMDK independent-oracle tests passed,
including hosted backing chains and external flat/split sparse profiles.
