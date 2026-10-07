# Native hosted sparse VMDK capacity changes

`VmdkWriter::resize(new_size, ShrinkPolicy)` changes the virtual capacity of a
standalone hosted sparse VMDK on Linux without converting its logical disk.
The existing writer requires 64 KiB grains, 512-entry grain tables, a bounded
embedded descriptor and an exclusive retained file lock. Capacity must be
nonzero, sector aligned and at most 32 GiB. External flat or split descriptors,
parented children and other writer profiles are rejected before CID mutation.
Callers must exclude dependent images and external readers throughout resizing.

Growth inside the current table coverage updates capacity and the descriptor's
extent sector count. Growth requiring additional tables prepares new grain
directories and tables in an expanded protected metadata arena. Only active
payload grains that intersect that arena are relocated, with their primary and
redundant mappings updated together. Metadata preparation uses pages of at most
1 MiB, within the existing sidecar journal's patch and encoded-size limits.
Primary and redundant coverage are both retained when present. Prepared pages
remain unreferenced until final header and descriptor publication.

Shrink requires an explicit policy. `Reject` refuses shrink; `RequireZero`
checks every removed logical byte before changing the CID or payload;
`AllowDataLoss` permits removing nonzero bytes. Removed grain mappings are
cleared in bounded groups. The remaining boundary grain's hidden suffix is
zeroed, and growth also clears any formerly hidden padding in existing images.
Regrowth therefore reads as zero rather than revealing removed content.
Neither guest partition tables nor filesystems are changed.

Every stage uses the existing durable Linux sidecar protocol and validates old
and proposed metadata views. A fresh canonical CID is synced before the first
mutation in an epoch. Descriptor edits preserve unrelated fields, retain short
CID widths and refresh the CID location if the extent line changes length.
The operating system lock stays held throughout; all fallible cache reads are
prepared before the final capacity transaction commits.

This operation does not promise one transaction for the entire resize. An
interruption can leave completed relocation or metadata-preparation stages at
the old capacity. A failed shrink can likewise leave a completed zeroing prefix
at the old capacity. A published sidecar must be recovered by reopening; the
failed live writer rejects further access. Final publication recovery selects
the proposed capacity. Host file truncation, hole punching, secure erasure and
storage reclamation are not provided: unreferenced physical bytes may remain.
Repeated growth across table boundaries can accumulate old metadata arenas.
Transactions hash the physical image, so large allocated disks can be expensive
to resize. New-output compaction remains available separately.

Host tests cover capacity growth across grain-table coverage, sparse growth to
32 GiB, shrink policies, hidden padding, zero-grain entries, retained locks,
parent rejection and CID placement after extent lines. Fault tests interrupt
each stage of relocation, metadata preparation, mapping release and final
publication. An independent native QEMU image with redundant tables is grown,
shrunk and regrown, then converted for exact logical byte comparison. These
tests do not establish native VMware runtime behavior or real power-loss
durability on arbitrary storage.
