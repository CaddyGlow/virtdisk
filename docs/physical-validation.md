# Physical container validation

`RawWriter::physical_fingerprint` computes a complete physical-file SHA-256 and
length through the writer's retained handle. The writer mutex and existing file
lock remain held throughout the scan. Local writer operations cannot interleave.
Callers must exclude noncooperating external mutation and validate path identity
separately. A digest grants no image authority and does not capture guest state.

`PhysicalValidationBudget` is independent of logical `OperationContext` quotas.
Validated limits default to, and cannot exceed, 33 GiB of requested read bytes,
1,048,576 exact-read calls, and 64 KiB of fallibly allocated scratch. The byte
ceiling matches the existing native journal physical profile; this change does
not establish throughput at that maximum. Zero byte/call limits allow empty files;
scratch limits must be between 1 and 65,536 bytes.

Whole-scan preflight checks cumulative requested bytes and calls before data reads
or scratch allocation. Refusal returns `Unsupported` carrying a typed
`PhysicalValidationLimitExceeded`. Invalid limits return `InvalidInput`. Changed
physical length before or after the scan returns `InvalidData`.

Usage persists across scans and failures. Requested bytes and exact-read calls
are charged before each attempt, conservatively including failed partial reads.
`hashed_bytes` counts only completed hashed chunks; `peak_scratch_bytes` records
the largest allocated buffer. Metadata operations, seeking, allocator overhead,
and internal system-call retries are outside these counters. Read-call limits
count Rust exact-read attempts, not kernel calls.

The scan changes neither content nor capacity and performs no flush or callback.
Existing journal hashes have not been migrated to this budget. Persistent graph
leaf deletion, its journal, recovery and destructive CLI remain unimplemented.
Fuzz campaigns and replay remain paused.
