# CLI error records

Place `--json-errors` first to request one JSON error record on stderr when a
command fails. It applies to all commands and to invalid subsequent controls.
Plain diagnostics remain the default. Successful result output and exit codes
are unchanged: success is 0, unequal comparison is 1, and command error is 2.

```sh
virtdisk --json-errors --operation-limit bytes=1048576 hash disk.raw raw
virtdisk --json-errors --progress check disk.qcow2 qcow2 payload
virtdisk --json-errors --recover zero disk.vhdx vhdx 0 512
```

Records contain these fields:

| Field | Meaning |
| --- | --- |
| `type` | Always `error` |
| `code` | Typed category when recognized; otherwise the I/O kind label |
| `kind` | Stable I/O kind label; unmapped categories use `other` |
| `message` | Escaped human-readable error, preserving source display text |
| `operation`, `format` | Typed mutation provenance when available, otherwise null |
| `offset`, `length` | Attempted logical range when available, otherwise null |
| `resource` | Resource name for typed common-context or parser-limit errors; otherwise null |
| `limit`, `requested` | Decimal strings for exact quota values, including u128 requests; otherwise null |

Typed codes are `resource-limit`, `parser-limit`, `recovery-required`, and `cancelled`.
Common-context resources are `logical-bytes`, `io-operations`, and `scratch-bytes`.
Shared parser resources are `metadata-bytes`, `cache-bytes`, `work-items`,
`decompressed-bytes`, and `decompression-buffer-bytes`. Chain/descriptor resources
are `recursion-depth` and `attribute-bytes`; future unknown resources
use `unknown`. Refused cumulative charges leave counters unchanged. Requested
parser usage records the atomic counter observed at refusal plus the charge,
which may exceed u64 without wrapping. Per-unit decode requests use unit size.
Generic codes use the kind label, including `invalid-input`, `invalid-data`,
`not-found`, `permission-denied`, `already-exists`, `unexpected-eof`,
`unsupported`, `interrupted`, `would-block`, `broken-pipe`, `write-zero`,
`out-of-memory`, `timed-out`, `storage-full`, `quota-exceeded`, `file-too-large`,
`read-only-filesystem`, `not-a-directory`, `is-a-directory`,
`directory-not-empty`, `resource-busy`, `invalid-filename`, and `other`.
Source traversal is bounded to 64 entries. The human message is not a stable
classification interface. Other profile-specific constraints retain their
generic kind code. Chain-depth and VMDK descriptor/table limits preserve their
existing `invalid-data` kind, while their code is `parser-limit`. Range provenance describes the attempted
operation; it does not establish rollback or identify every partially written byte.

With `--progress`, progress records can precede the error on stderr. Each record
has its own `type`; errors preserve stdout for results. Failed diagnostic writes
are handled without panicking, although a closed stderr cannot receive a record.
An unequal comparison emits no error record. These development additions are
unpublished after registry 0.2.0.
