# Opened-handle capability reports

This is a development addition after published 0.2.0. `ImageCapabilities::iter()`
enumerates each current operation once without allocating. It returns the same
`Capability` as `get(operation)`. `ImageOperation::as_str()` and
`UnsupportedReason::as_str()` provide stable machine-readable labels; structured
operation errors use the same operation names.

The report describes the concrete opened handle. It does not predict whether a
format family or a different profile could be opened writable. `Supported`
means implemented within the documented profile, platform, alignment, capacity,
dependency and transaction bounds; a specific request can still fail. Obtain a
fresh inspection after mutations to update snapshot counts, capacity and
capabilities. A retained inspection value remains a point-in-time report.

The existing conservative refusal categories are:

| Reason | Meaning |
| --- | --- |
| `read-only-handle` | The queried positional mutation requires a writer. |
| `not-implemented` | The current profile/platform/state does not expose this native operation. |
| `allocation-unknown` | The current handle does not expose allocation classifications. |

`not-implemented` does not identify every platform or profile constraint. Refer
to each operation's contract for those bounds. No capability grants permission,
validates a proposed range, guarantees host allocation/durability, or authenticates
an image. A supported discard does not establish guest filesystem free space.

Native handle operations and generic new-output management are distinct. For
example, native `compact`, `derive`, `rebase` and `merge` may be unavailable on
an opened reader/writer while new-output compaction and graph materialization
remain available through their separate APIs. This report does not describe
those APIs as unsupported. Native internal snapshot entries describe the
existing QCOW2 disk snapshot profile; they do not promise VM-state capture.

## CLI

```sh
virtdisk capabilities read disk.qcow2 qcow2
virtdisk capabilities write disk.qcow2 qcow2
virtdisk capabilities read child.vdi vdi parent.vdi base.vdi
virtdisk --parser-limit metadata=1048576 capabilities read disk.vhdx vhdx
```

An explicit `read` or `write` argument selects the handle to inspect. Read mode
uses the common immutable opener, with explicit parent/extent authorization,
leading parser limits and optional immutable VHDX log replay. Unknown raw input
requires explicit raw selection as usual. Write mode uses the explicit clean-only
writer opener, retains its exclusive lock while reporting, and accepts authorized
parent/extent paths. Opening and reporting do not modify image bytes. Nonempty
dependency lists for raw writers are invalid; an explicitly empty library list
is accepted.

Write mode rejects pending sidecars/logs with the normal recovery-required
error and preserves evidence. `--recover` is rejected for this command: recovery
must be performed as a separately authorized operation. Writer opening can fail
because the platform/profile is unavailable, a lock is held or dependencies are
not authorized; no capability report is produced for an unopened handle. Leading
reader parser/replay controls are rejected for write mode before file access.
Operation quota/progress controls do not apply to either mode.

Success writes one JSON object followed by a newline to stdout and exits 0:

```json
{"type":"capabilities","scope":"opened-handle","access":"read-only","profile":{"format":"raw"},"validation":"regular-file","virtual_size":512,"has_parent":false,"native_snapshots":null,"operations":[{"operation":"read","supported":true,"reason":null}]}
```

The example abbreviates `operations`; the actual report includes all current
operations, including unsupported entries. Each entry has `operation`, boolean
`supported`, and a nullable `reason`. Profile fields expose the facts retained
by `ImageProfile`: QCOW2 version, VDI dynamic/fixed layout or VMDK descriptor
presence where applicable. Validation reports opening scope, not a new payload
sweep. `info` supplies additional geometry and container-size facts.

Consumers should key entries by operation label and tolerate additional entries
and fields in future releases. Refused opening or invalid controls return exit 2;
place `--json-errors` first for the existing structured error contract. A stdout
write failure returns an error without panicking, releases the writer handle and
can leave a partial report on stdout. It does not mutate image bytes.
