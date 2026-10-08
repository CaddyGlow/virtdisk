# Immutable reader opening options

These development APIs are not included in the published 0.2.0 artifact.

`Image::open_with_options` accepts `ReaderOpenOptions` with an optional explicit
format, authorized dependency paths, validated parser limits and a typed
`ReadRecoveryPolicy`. Defaults select automatic binary recognition, standalone
opening and rejection of pending recovery. Unrecognized data requires explicit
raw selection. A malformed recognized container never falls back to raw.

```rust,no_run
use virtdisk::{Image, ImageFormat, ParserLimits, ReaderOpenOptions};

let options = ReaderOpenOptions::default()
    .format(ImageFormat::Qcow2)
    .authorized_paths(["parent.qcow2".into()])
    .parser_limits(ParserLimits::default())?;
let image = Image::open_with_options("child.qcow2", &options)?;
# Ok::<(), std::io::Error>(())
```

Private option fields preserve validation. Caller limits cover parsing,
authorized parents and deferred reads through the format's shared parser
budget. Raw deferred reads also retain a budget. Recognition uses a separate
fixed bound of 68 bytes. QCOW2 charges its fixed header as metadata even when
there are no extensions or backing names.

Shared-accounting refusal carries `ParserLimitExceeded`, with a typed
`ParserResource`, ceiling and u128 requested usage. Deferred-reader provenance
keeps this payload in its error source chain. Failed charges do not advance
their counters; cache reservations remain releasable. These structured errors
cover shared metadata, cache, work, cumulative decode and decode-unit limits.
Authorized QCOW2/VHDX/VDI/VMDK chain-depth checks and VMDK descriptor/grain-table
byte ceilings also carry structured parser errors. Depth counts include the
child; the effective limit is the smaller caller and profile ceiling (32 images
for QCOW2/VHDX/VDI, 64 for VMDK). Profile checks retain their `InvalidData` kind;
shared-accounting refusal retains `Unsupported`. Profile bounds have no
cumulative counter and do not undo earlier accepted accounting. Cycles, empty
required descriptors, and native-layout corruption retain their existing
validation errors. These structured additions are unpublished after 0.2.0.

An authorization list selects the chain opener. Empty lists authorize no
dependencies; omission selects standalone opening. VDI ancestors are ordered
from direct parent to base. Text VMDK descriptors require explicit VMDK
selection and authorization of external extents. Raw rejects nonempty lists.

`ReadRecoveryPolicy::ReplayVhdxLog` validates and overlays the native VHDX log
in memory. It never writes the source or authorizes native recovery. Other
formats reject this policy. The overlay may extend its addressable source
beyond physical EOF; common image inspection still reports the actual child
file size, excluding parent files. All source files must remain immutable for
the reader's lifetime. Existing `open` and `open_chain` retain their contracts.

## CLI controls

Leading `--parser-limit NAME=INTEGER` controls apply to input readers for
`info`, `hash`, `map`, `convert`, `compact`, `compare`, copy-based `resize`
and `capabilities read`.
Names are `metadata`, `cache`, `recursion`, `work`, `decompressed`,
`decompression-buffer`, `attribute`, and `attribute-list-records`. Byte fields
use bytes; the other fields use counts. Values must be positive and at most
the corresponding library hard default. Repeating a name retains its smallest
value, and separate names can be combined. Invalid controls are rejected before
opening files. Parent arguments retain their existing explicit authorization.

```sh
virtdisk --parser-limit metadata=1048576 --parser-limit work=100000 info child.qcow2 qcow2 parent.qcow2
virtdisk --replay-vhdx-log hash disk.vhdx vhdx
```

Leading `--replay-vhdx-log` selects immutable native log replay for the same
commands. It can be combined with parser limits in either order. Non-VHDX
readers reject this policy. All opening limits stay attached to deferred reads.
Comparison applies the selected ceilings separately to its two input graphs.
Conversion, compaction and resize apply these ceilings to their input reader;
exporters and reopened output validation retain their own default bounds. These
flags are not a total management-operation quota.

`check` and read-only QCOW2 snapshot `list`/`export` also accept parser limits;
they reject immutable log replay before opening files. Checking retains one
parser budget through ownership validation and optional payload reads. Snapshot
limits remain attached to saved views. Other commands reject these controls
rather than ignoring them. Native mutation retains separate resource-control
work. `--recover` remains the distinct mutating writer policy. These CLI controls
are unpublished changes after 0.2.0.
