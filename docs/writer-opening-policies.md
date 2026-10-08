# Explicit writer opening and recovery

This API is a development increment after the published 0.2.0 checkpoint; it is
not included in that registry artifact.

CLI commands that open an existing mutable image now use this policy too:
`zero`, `trim`, `preallocate`, `resize-native`, and QCOW2 snapshot
`create|delete|revert`. They reject pending recovery by default. A leading
`--recover` explicitly authorizes supported validated redo under retained locks
before performing the command. The flag is rejected for read commands and
commands creating new outputs; existing parent arguments remain required.
Recovery may change the image even if the subsequent operation fails, and a
failed recovering open does not promise rollback. CLI recovery policies are
also unpublished development changes.

`ImageWriter::open_with_options` selects dependency authorization and recovery
with `WriterOpenOptions`. Its default is standalone access with pending recovery
forbidden. Existing `open` and `open_chain` methods retain their established
behavior: QCOW2/VDI/VMDK may recover supported journals, while VHDX requires
separate explicit recovery.

```rust,no_run
use virtdisk::{ImageFormat, ImageWriter, RecoveryPolicy, WriterOpenOptions};

let options = WriterOpenOptions::default()
    .authorized_paths(["parent.qcow2".into()])
    .recovery_policy(RecoveryPolicy::Recover);
let writer = ImageWriter::open_with_options("child.qcow2", ImageFormat::Qcow2, &options)?;
# Ok::<(), std::io::Error>(())
```

`RejectPending` refuses recognized transaction evidence without replay, image
mutation or evidence cleanup. It returns `InvalidData` with a typed
`RecoveryRequired` payload when the opener reaches a recognized pending state.
Malformed images, inaccessible dependencies and lock conflicts can fail earlier
with their own errors. The policy does not promise that every corrupt image is
classified as recoverable.

`Recover` authorizes supported, validated redo under retained exclusive image
locks. Dependency authorization remains required; recovery does not repair
arbitrary corruption, invent missing journals, authorize new paths or enable
unsupported profiles. VHDX native recovery transfers the same locked file into
the writer. There is no unlock/reopen interval between redo and writer creation.
Split sparse VMDK checks descriptor/extent transaction evidence after retaining
all mutable participant locks and before replay or marker publication. Flat VMDK
transaction replay remains unsupported.

An unsuccessful recovering open may leave partially replayed metadata, even if
no writer is returned. Retry the supported recovery path before normal access;
there is no rollback guarantee. All operations still require exclusion of
non-cooperating external writers. Raw images have no container recovery protocol.

The authorization list is explicit and retained as owned paths in the reusable
options value. VDI paths are ordered from direct parent through base; other
formats resolve only listed dependencies. Supplying an empty list selects chain
resolution without authorizing any dependency. Omitting the list selects the
standalone profile. Explicit raw writer options reject nonempty dependency lists instead of ignoring
them. An explicitly empty list remains valid. Opening options do not alter
parser ceilings or native
writer capacity/platform bounds.

Behavioral tests cover clean-only refusal without changing complete directory
contents, explicit redo through existing interruption cuts, authorization,
parent immutability, whole logical byte models, idempotent reopen and exclusive
lock retention. Host interruption tests and cross-compilation do not establish
physical power-loss behavior or Windows runtime acceptance.
