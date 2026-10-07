# Native VHDX differencing implementation

This extends the clean standalone reader and native BAT redo writer. Parent access is an explicit operation; opening an arbitrary byte source continues to reject a child that requires a parent.

## Format contract

Follow the [Microsoft parent locator](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/b6332a98-624d-46b8-bd0e-b77b573662f9), [BAT layout](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/af7334e6-ad2c-4378-9b81-afc1334a6ee7), and [payload states](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-vhdx/01da203b-b3d7-487d-928b-22a460bbe177). A child has HasParent, a required parent locator, and a bitmap entry after each complete chunk of payload slots, including padding in the final chunk. Parent linkage is the parent's active DataWriteGuid. Logical sectors must match.

NotPresent inherits parent bytes; Zero masks them. Undefined and Unmapped read as zero under the existing permitted policy. FullyPresent uses private payload. PartiallyPresent resolves every logical sector through its chunk's bitmap: bit one reads private payload, bit zero reads parent. Retained offsets in zero states remain owned allocations. Bitmap allocations cannot alias payload, metadata, another bitmap, or logs.

## Authorization and resource bounds

`Vhdx::open_chain(path, authorized_parent_paths)` resolves locator paths in their native priority, canonicalizes candidates, and requires authorization before opening parent data. Every opened handle identity and canonical path is tracked, preventing hard-link cycles as well as path cycles. Depth, metadata, cache, and deferred work share one budget across the whole chain. Unknown locator types, malformed UTF-16, duplicate keys, overlapping strings, invalid GUIDs, and incomplete required metadata fail closed. Read-only opening never rewrites stale locator strings.

Relative paths use native backslash separators. Unix relative resolution rejects drive, UNC, protocol, and alternate-stream syntax. Windows-only absolute and volume locator forms require their native namespace and authorization. The same native identity checks must run on Unix and Windows; compilation alone does not prove Windows behavior.

## Creator and writer stages

First validate parent and its complete authorized chain, retaining its read handle. Create a new native child with an empty BAT, correctly padded bitmap slots, required locator, and fresh file/data GUIDs. Preserve virtual-disk metadata from the parent according to the IsVirtualDisk copy rule; do not invent a new Page83 identity for the same virtual disk. Use create_new and durable publication.

Writable open locks the child and validates linkage before mutation. Writes to inherited or partially present payload use native sector-bitmap allocation and updates, with payload synced before atomic BAT-plus-bitmap metadata publication. Arbitrary byte writes preserve the affected logical sector edges. Explicit ZERO replacement initializes a complete private block so untouched bytes remain zero. Whole-block discard can log ZERO directly. See [partial-sector writes](vhdx-partial-write.md) for redo ordering, resource limits and failure behavior.

## Validation gates

Tests precede each stage: locator boundaries; authorization/linkage/depth/identity; all read states and bitmap chunk/final-slot bounds; child creation identity and virtual metadata; copy-on-write preservation; existing native allocation crash cuts and idempotent recovery with a parent. Independent tools are evidence only for formats they actually support. QEMU VHDX differencing support must be checked before asserting an oracle; standalone flattening is a different gate. Actual Windows-produced children and Windows crash recovery remain explicit external validation gates.

## Implemented checkpoint

The chain reader, native creator, sector extent mapping, native partial-sector writer, immutable child recovery view, and locked writable child recovery are implemented. Public entry points are `Vhdx::open_chain`, `open_recovered_chain` (and limit variants), `create_vhdx_overlay`, `VhdxWriter::open_chain/create_overlay`, and `recover_vhdx_chain`. Parent authorization pins opened file identities in addition to canonical paths. Relative locators work across supported hosts; native volume/absolute Windows locators require Windows. The initial chain profile requires equal virtual capacity and equal logical sector size; payload block sizes may differ. Dirty parents require their own explicit recovery before a child is opened.

Creation uses 1 MiB payload blocks, a BAT capped at 64 MiB, and a metadata region capped at 1 MiB. All optional IsVirtualDisk metadata items and Page83 identity are copied. Literal backslashes in Unix filename components cannot be represented by a native relative locator and are rejected before destination creation. Child file/data identities are fresh. Creation preserves existing destinations and keeps its lock continuously when returning a writer.

Focused validation includes ten integration tests (4096-byte sectors, native partial bitmap inheritance, optional virtual metadata, shared limits/deeper chains, parent authorization, hard-link cycles, COW, zeroing, alias locks, reopen) and seventeen COW interruption cases across first/reused native log epochs, including interruption during streaming copy. Immutable recovery leaves child source bytes unchanged; unauthorized writable recovery leaves child bytes unchanged; authorized redo is idempotent and preserves parent bytes. Existing standalone VHDX reader/export/writer/log/recovery oracle tests also pass.

The installed `qemu-img` refuses native child opening with `Operation not supported`. The ignored oracle test records that missing gate separately and checks/converts a standalone flattened export to independently verify its logical data. This does **not** establish native child interoperability. Windows-produced differencing fixtures, actual Windows chain opening, and power-loss recovery remain unfulfilled external gates. Native partial writes and bitmap transactions are implemented; mixed-capacity parent chains remain a future native profile. Native multi-descriptor QEMU log replay is separate from native child acceptance.

Graph staging uses crate-private creator/reader helpers with an explicit final locator directory. They retain the actual staged source handle and opened identity while calculating/resolving parent paths against the destination directory. A dedicated test validates the child before publication and after hard-link publication into that directory, preventing relative-path relocation failures.
