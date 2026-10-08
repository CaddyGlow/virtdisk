# External snapshot graph manifests

`ImageGraph::manifest` captures registered paths, formats, direct parent edges
and an optional selected state after revalidating the live graph. Selection
means choosing disk content through `reader`; it does not overwrite images.
`GraphManifest::save` persists that declaration as a new file. `GraphManifest::open`
parses it without opening any embedded image paths. `open_graph` requires the
caller to authorize exactly every registered path, then applies the normal graph
identity, parent metadata, family, alias, cycle and depth checks.

```rust,no_run
use std::path::PathBuf;
use virtdisk::{GraphManifest, ImageGraph, ImageSpec, ImageFormat};
# fn example() -> std::io::Result<()> {
let base = PathBuf::from("base.raw");
let child = PathBuf::from("snapshot.qcow2");
let mut graph = ImageGraph::open(&[ImageSpec {
    path: base.clone(), format: ImageFormat::Raw, parent: None,
}])?;
graph.snapshot(&base, &child)?;
graph.manifest(Some(&child))?.save("graph.manifest")?;
drop(graph);
let declaration = GraphManifest::open("graph.manifest")?;
let graph = declaration.open_graph(&[base, child])?;
let state = graph.reader(declaration.selected().unwrap())?;
# let _ = state;
# Ok(())
# }
```

The manifest is a topology declaration. It does not authenticate disk content,
pin identities across sessions, discover children elsewhere, or atomically
commit image updates together with the manifest. Reopening freshly binds file
identities. Raw images have no native parent identifier to validate. Callers
must declare every dependent before destructive operations and exclude
concurrent image/directory/graph changes. Missing or additional authorized paths
are refused, so a stale manifest cannot silently omit a child the caller lists.
Absolute paths must still match their authorized canonical locations; relocation
and external VMDK descriptor extent ownership remain unsupported by this graph.

Saving never overwrites an existing destination. A private staging directory
holds the new regular file, which is fully written and synced before hard-link
publication. Unpublished staging is cleaned up on a best-effort basis. A directory
sync failure may be reported after publication. Updating a persisted graph means
saving a new manifest generation after the caller completes image operations;
there is no automatic journal spanning image and manifest files. A previously
captured declaration remains a declaration of its original topology.

## Version 1 representation

All integers are unsigned little-endian. The complete regular file is between
48 bytes and 1 MiB. The parser checks the file size before bounded, fallible
buffer allocation, detects a changed read length, verifies the checksum, and
then validates the records. A checksum detects accidental corruption; it is
not an authentication signature. Unknown versions and unavailable native path
encodings return `Unsupported`. Invalid structure returns `InvalidData`.

| Field | Bytes | Meaning |
| --- | --- | --- |
| Magic | 8 | ASCII `VDGRAPH` followed by NUL |
| Version | 4 | `1` |
| Image count | 2 | `0..=128` |
| Selected index | 2 | Record index, or `65535` for no selection |
| Records | Variable | Exactly the declared image count |
| Checksum | 32 | SHA-256 of all preceding bytes |

Each record contains the following fields. Parent indices may refer forward or
backward; cycles and paths longer than 32 images are refused before image I/O.
Raw images cannot declare parents; QCOW2 accepts raw/QCOW2 parents, while native
VDI, VHDX and hosted single-file VMDK require their own container family.

| Record field | Bytes | Meaning |
| --- | --- | --- |
| Format | 1 | Raw `0`, QCOW2 `1`, VHDX `2`, VDI `3`, VMDK `4` |
| Path encoding | 1 | UTF-8 `0`, native Unix bytes `1`, native Windows UTF-16LE `2` |
| Parent index | 2 | Record index, or `65535` for no parent |
| Path length | 4 | `1..=65536` bytes |
| Path | Variable | Absolute path; NUL and duplicate paths are refused |

Unicode paths use UTF-8. Non-Unicode Unix names and unpaired Windows surrogates
retain their native encoding and require that platform when loading. Absolute
path syntax and authorization remain platform-specific even for UTF-8 paths.
Trailing payload, invalid indices, unknown formats, unsupported encodings,
truncation and checksum mismatch are refused. No supplied manifest path grants
access: only caller-supplied paths are canonicalized before authority comparison.
The immutable manifest value exposes borrowed image records and selected paths;
its fields are private, and binding live files is an explicit separate action.

## Cumulative graph parser limits

`ImageGraph::open_with_limits` and `GraphManifest::open_graph_with_limits` retain
one `ReadBudget` across registered images, ancestors, repeated `reader` calls
and deferred reads. Limits are validated before image/authority path access.
Registration counts graph records and path metadata, including canonical names
that may be longer than supplied symlinks. Manifest binding also counts the
caller-authorized path set. Identity revalidation and native parser work consume
shared work items. These counters describe bounded parser work, rather than an
exact trace of filesystem syscalls.

`ImageGraph::budget` exposes a cloned accounting handle for usage inspection.
Failed operations retain accepted cumulative charges. Cache reservations follow
live native readers, and their accounting survives dropping the graph. Invalid
limits fail before path I/O; exhausted ceilings retain typed
`ParserLimitExceeded` sources. A graph cannot reset its allowance by reopening
a node or reading through an inherited parent.

External snapshot and new-output rebase validation use the same budget for
staged child readers, including VHDX's publication-location parent resolution.
A later parser refusal removes unpublished staging and registers no child edge;
previously completed difference writes and operation-context usage may already
exist in the disposable stage. Original states remain immutable.

Parser budgets and `OperationContext` payload quotas are independent. Native
creation/writer allocation, recovery and filesystem implementation work keep
their own backend bounds. Generic flattened-output materialization validates its
independent output through the existing exporter/parser contracts, while its
graph source remains charged to the retained graph budget. Manifest framing and
serialization retain their fixed version-1 bounds. Existing `ImageGraph::open`
and `GraphManifest::open_graph` preserve legacy per-reader limits; their graph
`budget()` returns `None`. Caller-tightened aggregate accounting is explicit.

## QCOW2 backing interpretation

A graph's registered parent format must match the native reader's resolved
backing interpretation. QCOW2 uses its backing-format extension when present;
without that extension the existing chain reader detects QCOW2 by signature and
otherwise reads raw bytes. The graph validates the result for each registered
edge, including intermediate ancestors, in both v2 and v3 chains. Merely matching
an authorized filename is insufficient when its interpretation differs.

To use a QCOW2 container file as opaque raw disk bytes, the child must explicitly
record `raw` in its backing-format extension. If it omits the extension and the
reader instead recognizes a QCOW2 parent, a graph declaring that parent as raw
is refused. Matching detected raw/QCOW2 declarations remain supported. Existing
native chain opening retains its format inference rules, and malformed recognized
containers do not fall back to raw.

Manifest authority does not override native interpretation. Reopening a graph
or operating on an existing graph revalidates resolved parent types; an altered
format extension that changes the interpretation prevents reader access, new
snapshots and leaf deletion before those operations can mutate graph files.
This validation does not add per-parent format overrides to native chains.

## Atomic snapshot generations on Linux

`ImageGraph::snapshot_generation` and its `_with_context` variant publish a
fresh directory containing `image` and `graph.manifest`. The manifest selects
the new child and records its final absolute location. Supported parents are
raw/QCOW2 for QCOW2 children, and matching VHDX, VDI or hosted VMDK families.
Existing graph images remain unchanged. Reopening still requires the caller's
explicit authorization for every declared image path.

An owned private sibling directory (mode 0700) holds preparation. The image,
manifest and staging directory are synced before `GenerationPublication`, the
last cancellation boundary. Linux `RENAME_NOREPLACE` publishes both names
through one directory rename and refuses existing destinations, including a
collision created at that boundary. Other platforms return `Unsupported`
before image or filesystem work.

After rename, the live graph registers the child before syncing the containing
directory. A subsequent sync error leaves the complete generation visible and
its child registered. Before rename, errors and cancellation remove owned
staging on a best-effort basis and do not register a child. Process termination
can leave private staging directories. Callers must exclude concurrent source
and directory mutation. These namespace and synchronization guarantees do not
establish actual power-loss correctness or provide VM memory snapshots.

The sibling layout preserves relative VHDX parent locator depth. This operation
creates a new generation; it does not relocate an existing graph, change an
existing branch, or atomically update unrelated image files. Operation payload
quotas cover snapshot verification; native creation and filesystem publication
remain outside that accounting. Graph parser accounting remains separate.

## CLI generation creation

```
virtdisk graph snapshot MANIFEST PARENT DIRECTORY FORMAT AUTHORIZED_IMAGE...
```

The input manifest describes the existing graph. Supply every existing image
path explicitly, including the parent; the manifest alone grants no file
access. The new directory must not exist. The command publishes `image` and
`graph.manifest` together, selects the new child, and reports a JSON success
record. The input manifest is preserved. Output errors after publication can
leave the completed directory present; inspect it before retrying.

Leading `--parser-limit NAME=INTEGER`, `--operation-limit NAME=INTEGER`,
`--progress` and `--json-errors` use the common controls. Parser limits apply
to graph authorization and native readers; payload quotas apply to verification.
Neither `--recover` nor `--replay-vhdx-log` is accepted. Linux is required by
the underlying generation publication protocol. Other graph actions and
in-place branch management are not exposed by this command.

## CLI graph materialization

```
virtdisk graph flatten MANIFEST SOURCE OUTPUT FORMAT AUTHORIZED_IMAGE...
virtdisk graph merge MANIFEST CHILD ANCESTOR OUTPUT FORMAT AUTHORIZED_IMAGE...
virtdisk graph rebase MANIFEST SOURCE PARENT OUTPUT AUTHORIZED_IMAGE...
```

All three commands require explicit authorization for every existing manifest
image. Flatten creates a standalone image. Merge first verifies that ANCESTOR
is an ancestor of CHILD, then creates a standalone image with CHILD's logical
content. Neither changes the ancestor or any other existing branch.

Rebase creates a new QCOW2 overlay over a registered raw/QCOW2 parent, copying
differences and verifying the complete logical result against SOURCE. The
input manifest remains unchanged. The output is a new image file; this command
does not publish an updated manifest or select a new branch. To reopen its
chain, authorize the parent chain explicitly. An atomic rebased generation and
in-place branch rebase remain separate work.

Leading common parser limits, operation limits, progress and JSON errors are
supported. Source and graph verification use shared parser accounting. Payload
quotas and cancellation govern materialization/verification before publication;
native allocation and persistence work remain outside payload accounting.
Existing outputs are refused. Recovery and VHDX log replay controls are rejected.
Success JSON names the method; a stdout error after publication can leave the
completed output present. Platform and output profile constraints follow the
corresponding library operations.

## Atomic rebased generations

`ImageGraph::rebase_generation` and its `_with_context` variant create a new
QCOW2 child over a registered raw/QCOW2 parent, preserving the registered
source's complete logical content. The source may be any supported graph reader
profile. Difference copying and complete verification finish before publication.
The new selected manifest retains all original nodes and declares the child's
final path and parent. Existing images and branch edges remain unchanged.

```
virtdisk graph rebase-generation MANIFEST SOURCE PARENT DIRECTORY AUTHORIZED_IMAGE...
```

The command uses the same explicit whole-graph authority and parser/payload
controls as other graph commands. The fresh directory contains `image` and
`graph.manifest`; the input declaration is preserved. Publication uses the
shared Linux-only no-overwrite generation protocol. Cancellation before rename
cleans private staging on a best-effort basis. A parent-directory sync failure
after rename leaves the complete output visible and registered in the live
graph; fault-injection tests exercise that error path for both snapshot and
rebase generations. This does not establish actual power-loss correctness.
Other platforms refuse generation APIs before I/O. Recovery/replay controls
remain rejected by the CLI. In-place branch rebase and atomic updates to an
existing manifest remain separate work.

## Persistent disk-state selection

```
virtdisk graph select MANIFEST STATE OUTPUT_MANIFEST AUTHORIZED_IMAGE...
```

Selection is persisted as a new immutable declaration. The state must already
be registered, every graph image must be explicitly authorized, and the output
manifest must not exist. All live graph identities and native parent edges are
revalidated before saving. Reopening the new manifest and reading its
`selected()` path accesses the chosen disk state. Selection preserves all
branches, existing image bytes and the input declaration. It does not restore
VM memory or change a hypervisor's currently attached disk.

The library exposes `ImageGraph::save_manifest` and `_with_context`; passing
`None` saves a declaration with no selected state. The contextual operation
provides `MetadataValidation` and final `Publication` cancellation boundaries.
No callback runs after manifest publication. Accepted payload usage is preserved
and this metadata operation consumes no payload bytes or I/O quota; graph parser
accounting remains cumulative. A directory sync or stdout error after publication
can leave the complete output manifest present. Concurrent mutation must be
excluded by the caller.

CLI parser limits, progress, operation controls and JSON errors are supported;
recovery and VHDX log replay remain refused. This versions caller-owned selection
without overwriting an existing declaration. Existing declarations can also be
replaced through the Linux expected-declaration operation below. Destructive
in-place branch operations remain separate transaction work.

## Graph inspection

```
virtdisk graph info MANIFEST AUTHORIZED_IMAGE...
```

Inspection requires explicit authority for every declared image and revalidates
live identities and native edges. It reads graph metadata without changing
image files or declarations. All graph readers open successfully before success
JSON begins; authorization, parser quota or identity refusal produces no success
record. Stdout failure during rendering can still produce a partial record.

The report contains `selected` (a zero-based manifest index or null) and `images`.
Each image has its manifest `index`, `format`, `parent` index or null,
`virtual_size`, `path_display`, `path_encoding` and `path_hex`. Sizes describe
logical disks, not container storage. Unicode paths are JSON-escaped in
`path_display`; non-Unicode paths use null rather than a lossy string. Native
paths are always preserved by `path_hex`: Unix bytes or Windows UTF-16LE code
units. Other targets label Rust's opaque OS-string bytes `rust-os-string`, which
are platform/toolchain dependent. Diagnostic paths do not confer authority or
provide a cross-platform relocation scheme.

Leading parser limits and JSON errors are supported. Parser accounting is
cumulative across graph opening and the readers used for reporting. Recovery and
VHDX log replay are rejected. Payload operation controls/progress are not
supported for this metadata-only inspection command. Reader validation precedes
streaming output, and the size vector uses bounded fallible allocation.

## Controlled leaf deletion

`ImageGraph::delete_snapshot_with_context` adds cancellation to the existing
owned-leaf deletion contract. `MetadataValidation` precedes path and graph
validation; `SnapshotDeletion` is the final callback before exclusive child
opening, identity revalidation and unlink. Base images and nodes with children
are rejected before that final boundary. Cancellation or parser quota refusal
leaves the leaf registered and its files unchanged. Identity replacement at the
final callback is refused before unlinking. Callers must declare complete
ownership of all dependents and exclude concurrent mutation; the graph cannot
discover unknown children elsewhere.

Once unlink succeeds, the graph drops the child before syncing its directory on
Unix. A directory-sync error can therefore leave the child removed and graph
updated; no observer runs after unlink and cancellation cannot restore it. The
error path is covered by fault injection, which does not establish actual
power-loss correctness. Payload usage is unchanged by metadata validation,
locking, unlink and persistence; configured graph parser accounting remains
cumulative. The existing `delete_snapshot` uses a default context.

Saved manifests are not rewritten by deletion. A declaration still naming the
deleted image cannot be reopened; saving a successor declaration is a separate
operation. Atomic persistent graph deletion and multi-file VMDK deletion remain
separate protocols. No destructive graph CLI command is added by this contract.
Platform filesystem/locking restrictions remain those of existing leaf deletion;
cross-target compilation does not establish native Windows deletion behavior.


## Atomic replacement of existing declarations on Linux

`GraphManifest::replace(path, expected)` and `replace_with_context` replace a
manifest only while its parsed declaration matches the expected declaration.
Valid native/Unicode encodings of the same path are equivalent for this initial
comparison. The original serialized bytes are retained and must remain exactly
unchanged at the final replacement boundary.
The old file is opened without following symlinks, must be singly linked and
regular, and retains a nonblocking exclusive lock. Nonregular files, aliases,
locks and stale declarations fail before mutation. A declared image path cannot
be used as the replacement destination. Paths and declarations confer no image
authority; callers bind the graph with explicit grants separately.

A private 0600 sibling file is written and synced under a retained directory
handle. `ManifestReplacement` then permits cancellation. Source, directory and
staging identities and source/staging content are checked again before a single
atomic rename. RAII removes only owned unpublished staging on a best-effort
basis. No callback runs after rename, and parent directory sync follows it. A
post-rename sync error leaves the complete successor visible; inspect the current
declaration before retrying because the old expected declaration is now stale.
File permissions/metadata from the old declaration are not preserved. Process
termination may leave private staging files. Actual power-loss acceptance remains
open; a successful host sync does not establish hardware guarantees.

Callers must serialize management and exclude noncooperating image, file and
directory mutations. The inode lock and expected bytes reject stale cooperating
updates; they do not provide a lock against arbitrary pathname writes. Payload
context usage remains unchanged because this is bounded metadata work, with the
existing 1 MiB manifest framing limit. Image contents and native parent edges are not
mutated or atomically committed alongside this declaration. Other platforms
return `Unsupported` before filesystem access or callbacks.

```
virtdisk graph select-in-place MANIFEST STATE AUTHORIZED_IMAGE...
```

The CLI first binds the existing declaration with exact image authority and
validates the selected registered state, then replaces the declaration using
that expected prior version. Parser budgets, progress, operation controls and
JSON errors apply; log replay and recovery remain refused. Selection persists a
caller-owned state choice without attaching a hypervisor disk. Existing
`graph select` remains the no-overwrite successor-file operation. A stdout
error after replacement can also leave the successor published.

Tests cover complete replacement, stale expectations, unchanged images,
cancellation cleanup, destination identity/content changes, locks, hardlinks,
symlinks, CLI authorization/progress and injected parent-sync failure after
publication. The latter verifies that the complete successor stays visible and
that retrying with the old expectation is refused.


Linux manifest opening uses a nonblocking descriptor before checking regular-file
framing, so FIFO paths are refused without waiting for a producer. Regular-file
reads retain the existing size, checksum and decoding limits. Regression tests
cover alternate valid path encoding, equivalent encoding changes at the final
boundary (still refused), and a FIFO CLI probe with bounded timeout/child cleanup.
