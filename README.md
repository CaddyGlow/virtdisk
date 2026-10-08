# virtdisk

Bounded virtual disk access and offline image operations. Partition tables
are parsed by `partmgr` and filesystem interpretation belongs to `disk-capture`.

## Image management implementation

The implementation plan is [docs/implementation-plan.md](docs/implementation-plan.md).
Work is ongoing; support is defined by concrete format profiles.

| Format | Read profile | Current write profile |
| --- | --- | --- |
| Raw | Regular files | Create, positional writes, zero, flush, grow/shrink; Linux host hole punching and preallocation |
| QCOW2 | Existing v2/v3 readers, authorized chains and audited disk-only internal snapshot views | Standard v3 64 KiB/16-bit refcounts; Linux sparse allocation, shared payload/L2 COW, authorized overlays, cluster discard, bounded native capacity changes, native snapshot create/delete/revert and redo recovery |
| VDI | v1.1 fixed/dynamic and authorized UUID-linked differencing | Native export and derived images; modification UUID epochs; Linux journaled sparse allocation, inherited-block COW, dynamic discard and bounded standalone dynamic resize |
| VMDK | Uncompressed hosted sparse v1/v2, redundant tables, authorized flat/split descriptors and parents | Native hosted sparse export and derived images; Linux journaled sparse allocation and inherited-grain COW; authorized monolithicFlat and twoGbMaxExtentFlat descriptor writes; Linux standalone and authorized backed split sparse writes, existing-table allocation and bounded missing-table creation using empty-extent arenas or unclaimed metadata padding; native hosted-sparse grain discard and bounded standalone resize |
| VHDX | Fixed/dynamic and authorized differencing with sector bitmap inheritance; explicit immutable log recovery | Native dynamic export and derived images; logged allocation, native inherited-sector bitmap writes, dynamic full-block discard, bounded standalone dynamic resize and recovery |

`Image::open` detects binary signatures and requires an explicit raw selection
for unrecognized data. Descriptor VMDKs require `Vmdk::open_descriptor` with
authorized extent paths. `InspectImage` reports geometry, profile, validation
scope, and operation availability for the actual opened handle.
`Image::open_chain` and `ImageWriter::open_chain` dispatch authorized chains.
VDI parent paths must be ordered from direct parent to base; other families
resolve references only among explicitly authorized paths.
Backed split VMDK writes retain shared read-only parent locks and bind recovery
to every ancestor file's identity, size, digest and topology. See
[the split parent contract](docs/vmdk-split-parent-cow.md).

`ReadAt` remains an immutable fixed-length reader contract. Writable handles
are separate and implement `WriteAt`; they retain exclusive OS file locks and
serialize operations. Cooperating locks do not exclude arbitrary external
mutation. All callers must exclude external image access during writes and
recovery. Payload writes may partially complete on I/O failure.

Development APIs added after registry 0.2.0 include
[explicit immutable reader opening options](docs/reader-opening-policies.md),
[explicit writer opening/recovery policies](docs/writer-opening-policies.md),
[typed writer operation errors](docs/common-operation-errors.md), and
[common operation contexts](docs/operation-contexts.md). Context-aware copy,
hash, comparison, checking, conversion, compaction and new-output resize support caller budgets
and borrowed progress/cancellation observers. These additions are not in the published 0.2.0 artifact.

[Capability reports](docs/capability-reports.md) enumerate native operations on
an opened handle. The CLI supports `capabilities read|write IMAGE FORMAT [PARENT...]`;
write mode retains the exclusive lock and refuses pending recovery without replay.

`convert_image` materializes a new image, syncs it, verifies logical bytes, and
publishes without overwriting an existing path. It requires host hard-link
support. `resize_image` writes a new flattened image; growth reads as zero and
shrink requires an explicit `ShrinkPolicy`. A zero tail does not establish
partition/filesystem safety. Logical compare/hash, cancellable copying and
bounded extent visitation are also available. `compact_image` creates a verified
new independent output that omits zero payload units; it preserves capacity and
nonzero content and does not interpret guest free space.

`create_qcow2_overlay` creates an external derived image over an explicitly
selected immutable raw/QCOW2 parent. `create_qcow2_overlay_with_chain` explicitly
authorizes deeper ancestors. `ImageGraph` provides caller-owned external snapshot
dependencies, leaf deletion, flattening, new-output merge and content-preserving
QCOW2 rebase. `snapshot_as` also creates native VDI, single-file VMDK and VHDX
children. Context-aware snapshot, flatten, rebase and merge variants share
budgets, progress and pre-publication cancellation; see
[operation contexts](docs/operation-contexts.md#external-snapshot-graphs).
`ImageGraph::manifest` and `GraphManifest` persist dependencies and optional
selection, then require explicit path authority when reopening; see
[graph manifests](docs/graph-manifests.md). Explicit `open_with_limits` and
`open_graph_with_limits` retain one parser budget across graph and ancestor reads,
reopening, and staged snapshot/rebase validation.
Callers must declare every dependent and exclude concurrent mutations.
Native QCOW2 disk snapshot creation uses the bounded Linux writer with authorized immutable parents
profile and preserves saved bytes through later active COW writes; see
[snapshot creation](docs/qcow2-snapshot-write.md) and
[snapshot deletion/revert](docs/qcow2-snapshot-lifecycle-design.md).
Additional native capacity profiles, broader split sparse VMDK metadata relocation
and management, and advanced profiles remain outstanding. Linux QCOW2 allocation journals impose
additional bounds and single-link restrictions; see
[the recovery contract](docs/qcow2-write-recovery.md).
Native hosted-sparse VMDK trim releases mappings and masks parents; it does not
promise host reclamation. See [VMDK discard](docs/vmdk-discard.md).
Standalone split sparse writes and bounded table creation use
[complete-set transactions](docs/vmdk-split-transactions.md). Backed split COW
is implemented for the bounded Linux profile documented in
[its contract](docs/vmdk-split-parent-cow.md).

`WriteAt::discard` guarantees zero-readable bytes with explicit
`RequireDeallocation` or `AllowZeroFallback` policy. Native support currently
includes Linux raw hole punching, eligible QCOW2 whole-cluster release, and
VDI dynamic allocation-unit release with physical tail truncation, and
VHDX dynamic whole-block ZERO mappings with inherited-data masking;
other profiles permit only explicit zero fallback. Container deallocation does
not guarantee host truncation or a particular amount of reclaimed storage.

The optional CLI is enabled with `--features cli`:

```sh
virtdisk create OUTPUT FORMAT SIZE
virtdisk create-sparse OUTPUT FORMAT SIZE
virtdisk check IMAGE FORMAT structure|payload [PARENT...]
virtdisk snapshot create IMAGE ID NAME
virtdisk snapshot list IMAGE [PARENT...]
virtdisk snapshot export IMAGE ID OUTPUT OUTPUT_FORMAT [PARENT...]
virtdisk preallocate IMAGE raw OFFSET LENGTH
virtdisk resize-native IMAGE FORMAT SIZE reject|zero-tail|allow-loss [PARENT...]
virtdisk info IMAGE FORMAT [PARENT...]
virtdisk capabilities read IMAGE FORMAT [PARENT...]
virtdisk capabilities write IMAGE FORMAT [PARENT...]
virtdisk map IMAGE FORMAT [PARENT...]
virtdisk hash IMAGE FORMAT [PARENT...]
virtdisk compare LEFT LEFT_FORMAT RIGHT RIGHT_FORMAT
virtdisk convert INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT
virtdisk compact INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT
virtdisk resize INPUT INPUT_FORMAT OUTPUT OUTPUT_FORMAT SIZE reject
virtdisk zero IMAGE FORMAT OFFSET LENGTH [PARENT...]
virtdisk trim IMAGE FORMAT OFFSET LENGTH require|zero-fallback [PARENT...]
```

Existing-image mutation commands reject pending recovery by default.
Place `--recover` before `zero`, `trim`, `preallocate`, `resize-native`, or
`snapshot create|delete|revert` to authorize supported validated recovery before
the requested operation, for example `virtdisk --recover zero disk.vhdx vhdx 0 512`.
Recovery failure can leave partially replayed metadata. The flag is rejected
for readers and new-output commands; it does not authorize parent paths.
These CLI recovery policies are development changes after published 0.2.0.

Input readers for `info`, `hash`, `map`, `compare`, `convert`, `compact`, and
copy-based `resize` and `capabilities read` accept leading `--parser-limit NAME=INTEGER` and immutable
`--replay-vhdx-log` controls. See [reader opening policies](docs/reader-opening-policies.md)
for supported names, units and input/output accounting boundaries.
`check` and read-only snapshot `list`/`export` accept parser limits too, while
rejecting immutable log replay.
`hash`, `compare`, `check`, `convert`, `compact`, `resize-native`, and copy-based `resize` accept leading `--operation-limit bytes=N`,
`io=N`, and `scratch=N` controls for their common payload context. See
[operation contexts](docs/operation-contexts.md) for accounting and bounds.
These commands and `zero` accept leading `--progress` to emit JSON-line progress
on stderr while preserving result output on stdout.
Place `--json-errors` first for structured error records across CLI commands;
see [CLI errors](docs/cli-errors.md) for codes and provenance fields.

Formats are `raw`, `qcow2`, `vdi`, `vmdk`, and `vhdx`. Resize size is a byte
count; shrink policies are `reject`, `zero-tail`, and `allow-loss`. Conversion
and resize create new files. Native resize supports raw, bounded Linux standalone/authorized backed QCOW2,
and bounded standalone Linux VDI, VMDK, and VHDX
profiles; callers must exclude dependent images and external readers. See
[QCOW2 resizing](docs/qcow2-resize.md) (including preservation of internal snapshots) and [raw allocation](docs/raw-storage.md).
See [VDI resizing](docs/vdi-resize.md), [VMDK resizing](docs/vmdk-resize.md),
and [VHDX resizing](docs/vhdx-resize.md) for their metadata and recovery limits.
[VHDX partial-sector writes](docs/vhdx-partial-write.md) describe native
differencing bitmap updates and their recovery contract.
`check` reports recognized container ownership separately from optional current
logical payload reads; it cannot authenticate bytes or interpret guest filesystems.
See [read-only checks](docs/check.md). Snapshot commands operate on QCOW2.
Creation accepts UTF-8 or `hex:HEX_BYTES` IDs/names and changes the selected container under its
writer lock. Listing emits byte identifiers/names as
hexadecimal JSON fields; export selects an exact UTF-8 ID or `hex:HEX_BYTES`,
audits ownership, and creates a verified independent output without overwrite.
Info, map, hash, zero and trim accept authorized parents. Zero and trim mutate
explicitly opened images; conversion and comparison commands currently open
standalone inputs.

Info preserves `container_size` as the primary file length and reports
`container_set_size` for the image's own descriptor and extent files, excluding
parents and recovery sidecars. Unavailable sizes are reported as `null`.

`ReadAt` provides checked exact positional reads with a fixed logical length.
`RawDisk` retains a read-only regular-file handle. `DiskView` retains and bounds
its parent. `Qcow2::open_chain` opens only explicitly authorized backing paths.
Opening validates headers and read ranges; before capture, call
`validate_active_mapping_and_compressed_payloads` to check active allocation
ownership and every compressed descriptor in the chain.

## Existing QCOW2 reader profile

| Feature | Behavior |
| --- | --- |
| QCOW2 versions | v2 and v3 standard L1/L2 mapping |
| Clusters | 512 bytes through 2 MiB; allocated, unallocated and explicit-zero |
| Backing | Explicitly authorized raw/QCOW2 chains; canonical identity/cycle checks; maximum depth 32 |
| Compression | Pure Rust deflate and zstd; bounded decoding, window and total-work limits |
| Refcounts | All seven widths, exact reconstruction, copied-flag consistency, shared L2/data and leak checks |
| Physical ownership | Disjoint metadata; no data/metadata overlap; unused final L1 tail need not be physically padded |
| Partitions | Primary/backup GPT consistency and CRCs, unique identities and checked bounds; basic MBR; 512/4096-byte sectors |

Disk-only internal snapshots support bounded listing, complete ownership audits,
and immutable saved-state reads. VM-state ownership remains unsupported;
snapshot creation, deletion, and revert use narrower Linux writable
profiles. Revert can materialize saved compressed data into private active
grains within its transaction budget; active compressed writes remain unsupported. See [QCOW2 snapshot reads](docs/qcow2-snapshots.md).
Encryption, persistent bitmaps, extended L2, external data,
dirty/corrupt images and unsupported mandatory metadata are rejected. Extended
MBR/dynamic layouts are rejected. Supporting the profile does not imply support
for every QCOW2 variant.

The caller must supply immutable inputs for all deferred reads. Read-only file
handles do not enforce snapshot isolation. Container structure and compressed
decoding do not authenticate ordinary disk content: QCOW2 data and deflate have
no intrinsic content checksum. The capture CLI records physical source hashes
before and after writing and refuses publication when they change.

## Validation

```sh
nix develop --no-write-lock-file path:./nix --command cargo fmt -- --check
nix develop --no-write-lock-file path:./nix --command cargo clippy --all-targets --all-features --locked -- -D warnings
nix develop --no-write-lock-file path:./nix --command cargo test --all-features --locked
nix develop --no-write-lock-file path:./nix --command cargo test --locked -p virtdisk --test qcow2 -- --ignored
```

The ignored tests require `qemu-img` as an independent oracle. They cover v2/v3
randomized range reads, seven refcount widths, compressed codecs across four
cluster sizes, explicit-zero masking in backing chains, shared table/data
ownership, and QEMU's minimally stored L1 table. Host regression and bounded
fuzz tests cover malformed/truncated inputs. These checks do not establish
Windows installation correctness; see the
conversion evidence ledger in the historical windows-uup workspace.

Actual Windows-native VHDX checks cover clean standalone/partial children and
reconstructed redo transactions at both supported sector sizes; see
[the Windows acceptance harness](docs/windows-acceptance.md) and
[the evidence ledger](docs/implementation-status.md). Actual power-loss testing
and broader native runtime profiles remain separate gates.

## Releases

CI checks formatting, Clippy and tests on Linux and Windows. A matching version tag runs validation, verifies and publishes the crate to crates.io, then creates its GitHub Release with the crate and SHA-256 checksums.
CI also runs the ignored QCOW2 integration tests against QEMU as an independent oracle.

`zero_image_with_context` uses native zero calls of at most 64 KiB with budgets,
progress and cancellation. CLI `zero` selects this mode when operation controls
or `--progress` are supplied; ordinary `zero` retains whole-call validation.
Cancellation or native refusal can retain a completed prefix. The wrapper owns
no scratch buffer; backend allocations, opening, recovery and flush are outside
its accounting. The library does not flush; the CLI flushes after success.

On Linux, `ImageGraph::snapshot_generation` publishes a new external snapshot
and its selected graph manifest together in a fresh directory. The contextual
variant supports bounded verification and cancellation before publication;
see [atomic snapshot generations](docs/graph-manifests.md#atomic-snapshot-generations-on-linux).

Create a snapshot generation from an explicitly authorized graph:

```sh
virtdisk --progress graph snapshot graph.manifest /images/base.qcow2 /images/next qcow2 /images/base.qcow2
```

For multi-image graphs, list all existing image paths after the output format.
See [graph CLI generation creation](docs/graph-manifests.md#cli-generation-creation).

Native `snapshot create`, `delete`, and `revert` also accept leading
`--operation-limit` and `--progress`. They count native calls and provide a final
cancellation boundary before mutation; backend metadata and journal work remain
outside these counters. See [operation contexts](docs/operation-contexts.md).

Native `trim` and `preallocate` accept leading `--operation-limit` and `--progress`
with cancellation before native dispatch. Successful requests count their logical
range and one native call; backend physical I/O remains outside those quotas.
