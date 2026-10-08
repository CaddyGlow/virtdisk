# Read-only image checks

`check_image(path, format, authorized_paths, CheckOptions)` validates the
explicitly selected container profile. `CheckOptions::default()` requests
structural checking. `CheckOptions { payload: true }` additionally reads every
byte of the current logical disk in chunks of at most 64 KiB.

Raw disks have no container metadata: their report says `RawLengthOnly`.
QCOW2 checking reconstructs allocation ownership and exact refcounts throughout
the authorized backing chain, including supported internal disk snapshots.
With payload checking it also audits compressed descriptors in current and
saved mappings. Unsupported VM-state snapshots or metadata owners cause an
error rather than an incomplete successful report. VDI, VMDK and VHDX use their
existing open-time metadata, mapping and recognized allocation ownership
checks. Their successful scope is `SupportedContainerOwnership`. This means
recognized supported data mappings and their protected metadata ranges, not
every optional extension's internal contents. For example, VHDX permits opaque
optional regions and metadata items: their declared ranges must remain valid
and disjoint, but their application-specific contents are not interpreted.
Unknown required features are rejected.

The report contains the selected format, current virtual size, structural scope,
and current logical bytes swept. A metadata-only check reports zero bytes swept.
Errors return no success report. Payload reads establish accessibility and, for
compressed QCOW2 descriptors, decoding structure; they cannot authenticate the
original contents. The sweep does not read every unused physical allocation or
every historical logical view. Guest partitions, filesystems, native hypervisor
boot behavior and recoverability after power loss are outside this operation.

All embedded references must be resolved by the existing explicitly authorized
chain constructors. For VDI, parent paths retain their required direct-parent
through-base ordering. Inputs must remain immutable for the entire operation;
read-only handles do not exclude unrelated writers. Checks never repair or
otherwise mutate the container, its parent or extent files.

`check_image_with_cancel` checks its predicate before opening, during QCOW2
ownership validation, after constructors return and before each payload chunk.
Other formats' bounded open-time parsers cannot be interrupted inside their
constructors. Cancellation returns `Interrupted`. The separate
`check_payload_with_cancel` helper accepts any `ReadAt` implementation and
preserves read errors and their provenance.

Regression tests cover every container family, immutable bytes, QCOW2 top-level
and parent refcount corruption, reference authorization, payload read failure
and cancellation between chunks. These host tests do not establish native
Windows servicing or capture correctness.

## Caller limits (development API)

`check_image_with_limits` accepts validated caller-tightened `ParserLimits`.
`check_image_with_limits_and_context` also accepts an `OperationContext` for
payload budgets, progress and cancellation. Limits are validated before opening
files or invoking the observer. A single parser budget covers opening,
authorized dependencies, QCOW2 ownership/compressed-descriptor validation and
deferred payload reads. Payload context accounting remains independent:
completed bytes count successful chunks, while I/O usage includes failed calls.
An error returns no success report, and inputs remain immutable.

```sh
virtdisk --parser-limit metadata=1048576 --parser-limit work=100000 check disk.qcow2 qcow2 payload
```

CLI `check` now accepts the same leading parser-limit controls as other readers.
Read-only QCOW2 snapshot `list` and `export` also accept parser limits, including
through saved views and authorized parents. These commands reject
`--replay-vhdx-log` before file access; neither checking nor snapshot inspection
authorizes recovery. Exporters and reopened output validation retain their
separate defaults. These additions are unpublished changes after 0.2.0.
