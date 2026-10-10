# Export and pipeline diagnostic evidence, 2026-10-10

These retained raw conversion runs use 16 MiB images, ten samples, 1 MiB
requested buffers and a 16 MiB operation budget on tmpfs. All listed total rows
include exact staged-output verification and publication. Profiling-disabled
samples are summarized below; profiling-enabled component rows remain in CSV.
The 1/2/4 worker configurations ran serially on a shared host, in one session.
This is a warm-memory diagnostic, not a storage result or a repeatability gate.

| Workers | Data | Policy | Samples | Median MiB/s | Range MiB/s |
| --- | --- | --- | ---: | ---: | ---: |
| 1 | dense | Default | 10 | 1185.5 | 886.9–1334.6 |
| 1 | dense | Sparse | 10 | 1677.9 | 948.0–1776.0 |
| 1 | 6.25_percent | Default | 10 | 1756.6 | 1676.5–1826.7 |
| 1 | 6.25_percent | Sparse | 10 | 1704.5 | 1513.2–1785.3 |
| 2 | dense | Default | 10 | 1590.9 | 1146.2–1899.8 |
| 2 | dense | Sparse | 10 | 1379.3 | 962.7–1736.2 |
| 2 | 6.25_percent | Default | 10 | 1831.5 | 1068.9–2190.8 |
| 2 | 6.25_percent | Sparse | 10 | 1856.0 | 1272.0–2132.8 |
| 4 | dense | Default | 10 | 2132.8 | 1560.7–2316.7 |
| 4 | dense | Sparse | 10 | 2086.3 | 1756.5–2304.4 |
| 4 | 6.25_percent | Default | 10 | 2046.4 | 1305.1–2452.2 |
| 4 | 6.25_percent | Sparse | 10 | 2114.6 | 1827.6–2462.1 |

The pipeline applies only to raw export. A worker prepares a bounded payload;
one coordinator writes, synchronizes, verifies and publishes it. Requested
worker counts for container exporters do not imply parallel execution there.
CPU time, RSS, physical I/O and cancellation latency were not captured with these
runs. The report manifest explicitly separates the later binary hash from the
unrecorded original build identity. These limitations preclude a performance
acceptance claim or changing the default worker count.

Existing `/data/cache/virtdisk-conversion-baseline-16m-20261010.csv`,
`virtdisk-conversion-streaming-16m-20261010.csv`, and
`virtdisk-conversion-baseline-256m-20261010.csv` retain an earlier slow-storage
attempt. They must not be merged with tmpfs timings. The requested additional
VDI/VHDX 16/256 MiB, 64/1024 KiB nonzero-run matrix and pipeline repeat sessions
were prepared but did not run: the tmpfs escalation call was interrupted while
awaiting approval. No result is inferred for those cases. A finite raw 2 MiB smoke covers requested buffers 64/256/1024/4096 KiB
with a 16 MiB budget and one sample per case, using the default temporary
directory on shared storage. All four completed successfully. These samples
establish execution and verification only; no buffer-size timing winner is
inferred. Command: `conversion --smoke --formats raw --buffer-kib K
--memory-mib 16`. The new smoke binary hash is in the manifest.

M5 source-read call/byte reductions follow from the streaming algorithm and its
regression counters, but independent scale measurements are still required.
M6 has finite diagnostic samples here; its repeat-session, storage, resource and
fault acceptance requirements remain separate. Every CSV is retained unchanged.
