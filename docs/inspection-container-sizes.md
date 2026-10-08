# Container size inspection

The inspection increment implements the retained size facts below. Existing
`ImageInfo.container_size` means the primary container file
length, not filesystem allocated storage. Preserve that meaning and the CLI's
existing `container_size` output, including for external VMDK descriptors.

## Public facts

`ImageInspection` exposes two optional fields:

- `container_size: Option<u64>`: primary retained container source/file EOF.
- `container_set_size: Option<u64>`: primary container plus this image's own
  extent files. Exclude parent images and transaction sidecars.

Single-file formats report equal values. A descriptor-backed VMDK reports the
descriptor EOF as `container_size` and the checked sum of descriptor plus all
own authorized extent EOFs as `container_set_size`. Count full flat extent
files, including unused prefixes and tails, rather than only the guest-visible
slice. Neither field reports host allocated blocks, compression savings, or
physical space reclamation.

For readers constructed over a non-filesystem `ReadAt`, these facts describe
the retained container source's bounded `len()`. They do not claim that a host
file exists or that a sliced source exposes the containing host file's full EOF.
Descriptor extents opened by the parser have their actual retained raw-source
lengths, before logical slicing.

`None` means the retained handle could not supply the fact, not zero bytes.
The current inspection trait is infallible, so fallible VHDX writer metadata
lookup or a poisoned state mutex returns `None` for both fields. Do not open a
replacement handle, fall back to virtual capacity, or panic to obtain a size.

## Concrete implementation seams

Extend `info.rs::report` to accept the two optional values and populate every
concrete implementation. Keep these facts outside `DiskGeometry`, which records
guest geometry. `ImageWriter` already delegates concrete inspection; `Image`
retains reader inspection under its immutable-source contract.

| Handle | Retained source and minimal getter |
| --- | --- |
| `RawDisk` | Existing captured `length` / `ReadAt::len()` supplies both values. |
| `RawWriter` | Existing `State.length` / `len()` supplies current EOF for both. |
| `Qcow2`, `Vdi`, `Vhdx` | Crate-private getter reads private `source.len()` for both. |
| `Qcow2Writer`, `VdiWriter` | Crate-private getter reads retained `raw.len()` for both. |
| Monolithic `Vmdk` | Private `source.len()` supplies both. |
| Descriptor `Vmdk` | Primary is `source.len()`; retain a checked own-container aggregate during parsing. |
| Monolithic `VmdkWriter` | Retained `raw.len()` supplies both. |
| Flat `VmdkWriter` | Primary is descriptor `raw.len()`; `flat::Flat` sums `extents[*].writer.len()`, then the outer writer adds descriptor EOF once. |
| Split sparse `VmdkWriter` | Primary is descriptor `raw.len()`; `Sparse.files` sums `files[*].raw.len()`, already including descriptor at index zero. |
| `VhdxWriter` | Lock existing `State.file` and obtain `file.metadata().len()`; metadata or mutex failure yields `None`. |

In `Vmdk::parse_descriptor`, start with descriptor source length and checked-add
each successfully authorized physical `extent_source.len()` before turning it
into a flat `DiskView` or sparse logical reader. Store the aggregate on `Vmdk`;
its `extents: Vec<(u64, Arc<dyn ReadAt>)>` otherwise erases physical length.
Initialize the field in every monolithic construction path as well. Parent
assignment must not add ancestor source sizes. Charge retained metadata through
the existing parser budget and reject overflow during open, not inspection.

Writer aggregation uses already retained handles and checked arithmetic. Return
`None` on aggregate overflow or unavailable size. Do not use path metadata or
open handles during inspection. Reading each retained EOF separately is a
bounded observation, not an atomic graph snapshot during a concurrent write;
document that callers must serialize mutation if they need an operation-boundary
report. This increment does not add a new concurrency or recovery guarantee.

Preserve `ImageInfo.container_size` and existing CLI `container_size` semantics.
CLI `info` adds nullable `container_set_size` from inspection. The existing
primary size remains numeric and retains its current meaning. Document the
distinction where CLI JSON fields are described; do not silently replace a
primary size with an aggregate. No parent, identity or recovery metadata scope
is added by this size increment.

## Current mutation facts

Raw resize updates EOF directly. QCOW2 allocation grows EOF; native virtual
shrink releases ownership but explicitly promises no EOF shrink. VDI allocation
grows EOF, discard can truncate the owned physical tail, and resize may relocate
metadata while retaining or growing EOF. Monolithic VMDK allocation and staged
resize can grow EOF; virtual shrink retains EOF. Split sparse allocation grows
the touched retained extent files. Flat writes preserve file EOF within the
current bounded profile. VHDX allocation appends payload, bitmap and log regions;
all remain part of container EOF. Virtual resize is not physical compaction.

Use actual retained EOF rather than deriving size from allocation counts or
virtual capacity. This also reports staging growth that remains after a failed
operation; it does not declare that such a handle is safe for further mutation.

## TDD ladder

1. Assert raw reader/writer reports both dimensions equal to actual EOF, including
   a zero-length file. For QCOW2, VDI, VMDK and VHDX sparse fixtures, choose virtual
   capacities different from container lengths and compare both facts against
   opened file metadata. Include a bounded in-memory `ReadAt` source.
2. Open an authorized descriptor VMDK with two extent files. Assert primary size
   equals descriptor EOF and set size equals descriptor plus both physical EOFs.
   A flat extent must have a nonzero offset and unused trailing bytes so summing
   logical `DiskView.len()` demonstrably fails. Include sparse descriptor extents.
3. Inspect writers before and immediately after first allocation without reopen.
   Compare retained observations against filesystem EOF; include split sparse
   allocation in both extents and verify descriptor is counted once.
4. Resize raw smaller and larger; both facts track EOF and virtual capacity.
   Shrink QCOW2 and monolithic VMDK virtually and assert virtual size changes
   while the retained EOF does not. Exercise growth requiring new metadata.
5. Discard an allocated VDI tail and assert the inspection shrinks immediately.
   Exercise VDI resize metadata relocation without assuming EOF equals virtual
   capacity. VHDX first allocation must include appended log/payload regions;
   inspect after further bitmap allocation where the profile supports it.
6. Open backed readers/writers and verify both fields exclude parent bytes.
   Preserve current primary `ImageInfo.container_size` for descriptor VMDK.
7. CLI JSON keeps the existing numeric primary `container_size` and adds the
   correct nullable `container_set_size`. Verify single-file equality and split
   inequality. Test the optional-value formatting without needing to provoke an
   operating-system metadata failure. Unit-test VHDX unavailable-fact handling
   only through a narrow meaningful failure seam, if one is added.
8. Verify parser aggregate overflow fails at open and writer checked aggregation
   cannot wrap. Existing alias rejection prevents repeated own files from being
   counted multiple times; retain that contract.

Targeted `tests/info.rs` cases cover every family, live allocation, logical
shrink, VDI tail discard, descriptor aggregate dimensions, bounded non-file
sources and parent exclusion. CLI tests cover raw and flat-descriptor JSON.
The ladder also records additional edge cases for later coverage; it is not a
claim that every listed fixture has run. Run repository-required formatting,
Clippy and tests before publication. These inspection tests do not establish Windows
servicing or capture correctness. Fuzzing remains paused.
