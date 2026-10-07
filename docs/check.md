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
