# Container metadata for forensic analysis

`virtdisk metadata` reads native container declarations from supported images
without modifying them. The Rust equivalent is `InspectMetadata::metadata()`.
This complements `info` (geometry and sizes), `map` (logical allocation),
`check` (supported structural validation), and `hash` (logical disk content).
It does not interpret guest partitions, filesystems, deleted files, or OS logs.

## Read metadata

Build the CLI with the `cli` feature, then select the format explicitly:

```sh
virtdisk metadata disk.vdi vdi
virtdisk metadata disk.vmdk vmdk
virtdisk metadata disk.vhdx vhdx
virtdisk metadata disk.qcow2 qcow2
virtdisk metadata disk.raw raw
```

Each command emits one JSON object. `scope` is
`effective-container-declarations`: values describe the opened reader, not an
authenticated account of past activity. Raw reports `native_metadata: null`.
QCOW2 reports its snapshot directory. VDI, VMDK, and VHDX expose the fields
described below. A parser failure returns an error rather than a partial report.
Unsupported or damaged profiles can therefore require a separate byte-level
examination of the acquired file.

Authorize dependencies explicitly when opening a chain or external descriptor:

```sh
virtdisk metadata child.vdi vdi parent.vdi base.vdi
virtdisk metadata child.vhdx vhdx parent.vhdx
virtdisk metadata disk.vmdk vmdk disk-flat.vmdk
virtdisk metadata child.qcow2 qcow2 parent.qcow2 base.raw
```

VDI parents must be listed in direct-parent-to-base order. Other formats resolve
their embedded references within the supplied authorization set. Include VMDK
extent files where required. Declared names never authorize opening a file.
Inspect each ancestor separately if its metadata is needed; the report describes
the selected container, not every file in its chain.

The same retained reader is used by the Rust API:

```rust
use virtdisk::{ContainerMetadata, Image, ImageFormat, InspectMetadata};

fn main() -> virtdisk::io::Result<()> {
    let image = Image::open("disk.vdi", Some(ImageFormat::Vdi))?;
    if let ContainerMetadata::Vdi(metadata) = image.metadata()? {
        println!("creation UUID bytes: {:02x?}", metadata.creation_uuid);
        println!("parent UUID bytes: {:02x?}", metadata.parent_uuid);
    }
    Ok(())
}
```

`Image::open_chain` and `Image::open_with_options` also support this API.
Concrete immutable `Vdi`, `Vmdk`, `Vhdx`, and `Qcow2` readers implement the trait.
It requires the `std` feature. Extraction shares the reader's parser limits;
repeated calls consume cumulative metadata and work quotas. Returned reports
are caller-owned allocations. Keep the source and all parents immutable during
opening, extraction, and deferred reads.

## Interpret the native fields

### VDI

The four `*_uuid_hex` fields are lowercase hex of the exact 16 bytes in the
header, with no byte-order conversion:

| Field | Meaning |
| --- | --- |
| `creation_uuid_hex` | Declared identity of this image |
| `modification_uuid_hex` | Declared modification epoch of this image |
| `parent_uuid_hex` | Declared direct parent's creation UUID |
| `parent_modification_uuid_hex` | Declared direct parent's modification UUID |

Standalone images have zero parent fields. For an authorized differencing
image, the parser checks both parent UUID fields against the supplied parent's
header. Matching fields support a linkage observation, but are not content
hashes and can be copied or edited. “Creation” and “modification” name identifier
roles; they do not establish creation or modification dates.

### VMDK

`descriptor.cid` and `descriptor.parent_cid` are eight-digit hexadecimal strings.
CIDs are 32-bit content epoch markers, not cryptographic hashes. `ffffffff` as
the parent CID declares no parent. CID equality alone cannot prove identical
content or a unique ancestor.

`properties` is an ordered array of `{key, value}` records containing accepted
native properties and vendor `ddb.*` properties. Whitespace around assignments
is trimmed; property values retain their quotes. Repeated vendor keys are
preserved. This is a parsed representation, not a verbatim descriptor copy:
comments, padding, and original spacing are not included.

`extents` preserves declaration order and exposes access mode, extent type,
filename, capacity in 512-byte sectors, and FLAT starting sector. SPARSE
`offset_sectors` is `null`. A declared filename or `parentFileNameHint` is a
recorded hint, not proof that a particular host file was used. Correlate it with
the files actually supplied and their acquired hashes. Concrete chain readers'
`resolved_parent_path()` identifies the parent opened on the current host.

A binary extent with no embedded descriptor reports `descriptor: null`.
This means no descriptor is present in that extent; an external descriptor may
hold the relationship and must be inspected separately.

### VHDX

`headers` contains slots at byte offsets 65536 and 131072. A `null` slot failed
signature or checksum validation. `active_header_offset` selects the sole valid
slot or the valid slot with the larger sequence number. Sequence numbers are
decimal strings in JSON to preserve all 64 bits in consumers that use floating
point numbers. They order header generations within an image; they are not
dates or a count of guest writes.

Each valid header includes file-write, data-write, and log GUID bytes. File-write
and data-write GUIDs identify write epochs; the latter participates in parent
linkage. `virtual_disk_id_hex` comes from the virtual disk identifier metadata
item and is separate from these header GUIDs. These roles follow the
[Microsoft VHDX header specification](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/39d641c2-093c-4d4a-8c9d-bd4b9fc2ff31).

GUID `*_hex` fields preserve native on-disk byte order. For example, bytes
`33221100554477668899aabbccddeeff` correspond to the conventional GUID string
`00112233-4455-6677-8899-aabbccddeeff`: reverse the first four bytes and the next
two pairs, then keep the final eight bytes in order. Do not apply this conversion
blindly to every format's UUID representation.

`parent_locator` preserves all accepted key/value pairs with decoded UTF-16
values, including keys the resolver does not use. Typical keys are
`parent_linkage`, `parent_linkage2`, `relative_path`, `volume_path`, and
`absolute_win32_path`. Linkage GUID text uses conventional GUID notation;
convert header bytes appropriately before comparing. Paths can describe a
different machine or stale location. An authorized open validates actual
linkage; locator text alone does not establish provenance.

If a native log requires replay, ordinary opening refuses it. Explicit
`--replay-vhdx-log metadata ...` uses an immutable recovery overlay. Its report
describes the effective recovered headers and metadata, potentially differing
from the acquired file. Record that option with the report and preserve the
original file and its hash; the command does not write replayed bytes to it.

## Timestamps

QCOW2 snapshot records expose `date_seconds` (seconds since the Unix epoch),
`date_nanoseconds` (fractional seconds), and `vm_clock_nanoseconds` (saved guest
runtime, not wall-clock time). The 64-bit runtime is a decimal string in JSON.
Binary snapshot IDs and names are losslessly encoded as `id_hex` and `name_hex`.
The Rust fields are available through both `metadata()` and
`Qcow2::list_snapshots()`.

```sh
virtdisk metadata disk.qcow2 qcow2
virtdisk snapshot list disk.qcow2
```

Recorded snapshot timestamps are claims from the image, not authenticated event
times. Clock errors, edits, and producer behavior can affect them; zero does not
prove an event happened at the epoch. Correlate these values with independent
records before constructing a timeline. No wall-clock creation/modification
timestamp is reported for raw, VDI, VMDK, or VHDX by this API. UUIDs, CIDs, GUIDs,
and sequence numbers must not be treated as substitute timestamps. Host file
timestamps and guest filesystem timestamps are separate evidence sources.

## Hash original files and logical content separately

`virtdisk hash` computes SHA-256 over the selected logical disk bytes, including
bytes inherited from authorized parents. It excludes container headers,
descriptors, unused container space, and physical layout. Different containers
can consequently have the same logical hash.

For acquisition, also hash each original file: selected container, all extent
files, every parent, and any separately acquired journal or sidecar. For example,
on a host with `sha256sum`:

```sh
sha256sum child.vmdk child-flat.vmdk parent.vmdk parent-flat.vmdk
virtdisk hash child.vmdk vmdk child-flat.vmdk parent.vmdk parent-flat.vmdk
```

Keep an inventory of paths, file lengths, hashes, acquisition details, selected
format, authorized dependencies, recovery options, and tool version. Read-only
handles do not prevent external writes or guarantee that host access timestamps
remain unchanged. Use immutable acquired copies or an appropriately protected
source. Structural checks establish the documented container validation scope;
they do not authenticate an image's content or history.
