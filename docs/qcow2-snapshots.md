# QCOW2 internal snapshot reads

`Qcow2::list_snapshots` reports saved-state directory metadata without opening
or modifying a saved state. IDs and names remain raw bytes. Listing validates
directory bounds, alignment, unique IDs, timestamps and saved L1 coverage;
it does not audit allocation ownership. Limits are 1024 entries and 1 MiB of
directory metadata, in addition to the caller's cumulative parser budgets.

`validate_active_mapping` reconstructs ownership across the active state and
every disk-only saved L1/L2/data mapping, including shared and compressed
payloads, and compares exact native refcounts. Inactive copied flags may be
stale; active copied flags must remain consistent. Unknown ownership features
and nonzero saved VM state return unsupported errors.

`Arc<Qcow2>::open_snapshot(id)` performs that audit before exposing a saved
disk view. The view uses the saved capacity and L1 mapping, retains the original
image and authorized backing readers, and supports reads and extent visitation.
All files must remain immutable for the view's lifetime. Opening does not
restore the active state or alter its bytes. Missing IDs return `NotFound`.

The CLI supports `snapshot list IMAGE [PARENT...]` and
`snapshot export IMAGE ID OUTPUT FORMAT [PARENT...]`. Listing emits hex IDs
and names to preserve arbitrary bytes in JSON. Export accepts a UTF-8 ID or
`hex:HEX_BYTES` and uses normal verified no-overwrite conversion. No hypervisor
configuration or guest memory state is changed.

Tests cover QEMU v2/v3 snapshot directories, sub-byte through 64-bit refcounts,
shared/COW mappings, stale inactive flags, compressed payloads, corruption and
saved/current byte separation. Native QEMU may omit padding after the final
directory entry; parsing checks its actual content and aligned traversal.
See `tests/qcow2_snapshots.rs`, `tests/qcow2_snapshot_validation.rs`, and the CLI
snapshot tests. These tests do not establish VM boot or memory-state recovery.

Native snapshot creation is a separate, narrower
[writable profile](qcow2-snapshot-write.md). Revert and deletion remain
implementation work. The read API itself grants no mutation capability.

Format reference: [QEMU QCOW2 specification](https://www.qemu.org/docs/master/interop/qcow2.html#snapshots).
