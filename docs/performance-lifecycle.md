# Lifecycle measurements and serialized raw I/O

`cargo bench --bench lifecycle -- --smoke` exercises every writable format with
exact whole-image verification. The default uses ten fresh 16 MiB images per
format and 1000 requests per request phase. `--format`, `--size`, `--requests`,
and `--samples` select smaller experiments; size is bounded to 4–256 MiB because
this diagnostic keeps an exact in-memory expected image. It is not a bounded
streaming production API or a multi-GiB storage benchmark.

CSV separates first allocation, 512-byte partial updates, Linux QCOW2 internal
snapshot COW, discard, grow, zero-tail shrink, clean reopen, explicit recovery
policy open, and verified compact/publication. Unsupported native operations and
snapshot profiles are reported as exclusions. Each modifying phase is followed
by a full byte check through the locked writer; after closure and compact it is
also checked through the immutable image reader. Writers and readers never
coexist. Payload changes between phases; every sample uses a fresh image.

Request latency includes each write call, including backend journal/sync work
inside that call. Phase elapsed time additionally includes the final explicit
flush. p95/p99 use nearest ranks only with at least 1000 request observations;
whole-phase lifecycle operations report no invented percentiles. Timer overhead
is printed. Container length and Unix `st_blocks` are separate columns; physical
read/write amplification is `unavailable`, because container growth cannot
establish journal, metadata or sync I/O volume. Flush policy remains explicit and
unchanged; there is no delayed transaction API.

`clean_recovery_policy_open` measures a clean image with recovery authorized. It
is **not pending-log replay or crash recovery**. Existing format fault/recovery
tests validate replay correctness separately; native Windows and power-loss
acceptance, dirty recovery latency, parent-chain COW beyond internal QCOW2
snapshots, and durable per-request storage percentiles remain evidence gates.
The Linux tmpfs smoke passed for all five formats; it establishes benchmark
plumbing and full byte checks, not native consumer or storage performance.

RawWriter now uses Unix positioned exact-read/write methods while retaining its
operation mutex, bounded range checks, exclusive OS file lock, resize ordering,
and explicit `sync_all`. Standard-library exact methods retain short-I/O retries
and failure behavior. Zeroing and fallback discard retain bounded buffers.
Windows keeps the serialized seek implementation, including sequential zero
writes, until platform runtime evidence supports a change. This removes one
seek call before each ordinary Unix raw read/write without introducing parallel
mutable access or changing durability.

The retained [experiment evidence](evidence/raw-positioned-20261010/manifest.json)
compares foundation `1e0b180` with the candidate on a shared Linux host. Ten
100000-request runs per variant used warm tmpfs, 16 MiB images, random 4 KiB
existing-allocation writes and one final flush, followed by exact final-state
verification. Three repeated process comparisons gave median candidate/baseline
throughput ratios 1.117, 1.160, and 1.309. They are repeated executions on one
host, not three independent quiet sessions. The ordinary `/tmp` comparison
(10000 requests, ten rounds) gave 26.93 versus 28.15 MiB/s and was dominated by
shared storage noise. These results support the limited syscall optimization;
they do not establish storage bandwidth, a general format speedup, or the M7
native/crash acceptance exit gate. Raw samples, commands and binary hashes are
retained; broader matched evidence is still required.

Actual dirty-recovery correctness fixtures also passed for QCOW2 (transaction
cuts 0–7 and the post-commit cut), VDI and VMDK allocation cuts 0–8, and VHDX native
persistence boundaries. [Diagnostic process timings](evidence/raw-positioned-20261010/dirty-recovery-tests.json)
retain exact test names, commands, stdout and test-binary identity. Elapsed times
were 0.244, 0.172, 0.025 and 0.444 seconds respectively on tmpfs. Each duration
includes fixture preparation, injected interruption, dirty recovery, byte/mapping
checks and repeated opening; it is not an isolated replay latency or a tail
percentile. These deterministic persistence-cut tests establish the exercised
recovery invariants, not sudden power-loss behavior on physical storage.
