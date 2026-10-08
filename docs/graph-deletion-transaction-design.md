# Persistent owned-leaf deletion transaction

Status: proposed protocol; production persistent deletion and recovery are not
implemented. Existing `ImageGraph::delete_snapshot` changes an in-memory graph
and one file, and must not be presented as an atomic persistent graph operation.
This protocol extends the common opening/recovery and manifest contracts rather
than editing hypervisor registrations.

## Public behavior and prerequisites

Delete one caller-owned dependent leaf while preserving all ancestors, siblings
and other declared states. Initially support Linux single-file profiles already
accepted by `ImageGraph`; reject bases, non-leaves, external VMDK descriptors,
hardlinks, native pending recovery and overlapping manifest/image/artifact paths
before mutation. The caller declares the complete graph and excludes external
access and all noncooperating mutations. Unknown external dependents cannot be
discovered or made safe by this protocol.

Bind the old declaration to an exact caller-authorized image path set. Build and
validate the successor declaration with the leaf removed. Preserve the selected
state when it survives. If it is the deleted leaf, require an explicit surviving
selection or explicit unselection; do not infer a new active disk silently.
The target file, manifest and directories retain exclusive/identity-checked
handles for their required lifetime. RAII handles locks and unpublished staging;
commit, recovery and sync failures are explicit Results, never work in Drop.

Expose a dedicated persistent deletion operation and recovery entry point with
`RecoveryPolicy::RejectPending` as the ordinary-open default. Recovery requires
the original complete path authority even if the leaf has already disappeared.
A pending record blocks ordinary persistent manifest management and graph binding;
parsing the record grants no path access. Metadata-only manifest replacement must
also refuse a pending graph transaction. Selection replacement now uses internal owned `LockedManifest`,
`PreparedManifest` and `PublishedManifest` states. Preparation consumes and
retains the old lock; publication consumes prepared staging and returns an
explicit visible-successor state; directory sync is a separate fallible method.
Old and successor handles remain locked through sync. Reuse these primitives;
do not reopen the manifest or reacquire its lock through another descriptor.

## Journal framing and authority

Use a versioned checksummed redo record, published without overwriting an existing
sidecar. Store exact old and successor manifest blobs, the explicitly granted
original leaf path, a nonce-derived sibling tombstone name, physical leaf length
and SHA-256, and retained file/directory identities needed for stale-object checks.
The successor is a validated single-leaf removal from the original, not an
arbitrary supplied graph. A checksum detects corruption; it grants no authority.

Bound framing to 3 MiB, each manifest to 1 MiB, each native path to 64 KiB and
image count to 128. Validate lengths, arithmetic, version, reserved bits, duplicate
names, graph removal/selection invariants and checksum before opening any image.
Derive/validate artifact basenames from the manifest and recorded nonce; refuse
records that redirect tombstones outside the authorized leaf directory. Resolve
caller grants first. A missing leaf grant is normalized through its authorized
parent directory and basename, without canonicalizing a journal-supplied foreign
path or treating the tombstone as a fresh implicit grant.

Hash the physical leaf through its retained handle with bounded fallible scratch.
`RawWriter::physical_fingerprint` now provides a separate bounded physical
validation budget; see [its accounting contract](physical-validation.md). The
33 GiB ceiling follows the existing native journal profile; maximum-size
throughput has not been established. Integrating this prerequisite with the
deletion journal and recovery remains pending. Logical payload quotas do not
cover native journal work.
Source identity plus digest protects stale recovery; inode identity alone does
not establish immutable data. Remount/identity ambiguities are refusal cases until
an explicit tested recovery profile exists.

## Roll-forward states and ordering

1. Validate authority, native graph/profile, deletion/selection, all identities,
   old declaration and leaf digest. Preflight all preparation budgets. Stage the
   successor declaration and redo record privately; sync their files.
2. Emit the final persistent-deletion cancellation boundary and revalidate all
   sources and staged artifacts. Publish the complete record without overwrite
   and sync the manifest directory. The operation is now recovery-requiring;
   later errors do not discard the journal or imply rollback.
3. Rename the owned leaf to its nonce-derived sibling tombstone without overwrite
   through retained directory handles, then sync its directory. Relative native
   backing metadata is preserved because the tombstone stays in the same parent.
4. Atomically replace the old manifest with the exact staged successor under the
   retained lock; sync its parent directory. Update the live graph and selected
   declaration after publication, before returning any later cleanup error.
5. Verify tombstone identity/content, unlink it and sync its parent directory.
6. Remove the owned journal and sync its directory. Only now report complete
   persistent deletion. No observer runs after journal publication; cancellation
   cannot interrupt the durable state transition.

Before record publication, errors leave source objects unchanged and remove
owned staging best-effort. Termination may leave unreferenced private preparation
files. After publication, errors retain the record for authorized roll-forward.
A process restart must inspect authoritative filesystem state; a recorded phase
is not proof that a rename, unlink or sync completed.

## Recovery decision table

| Original leaf | Tombstone | Manifest | Action |
| --- | --- | --- | --- |
| Matching original | Absent | Exact old | Recheck sources; rename leaf, then publish successor and clean up |
| Absent | Matching original | Exact old | Publish successor, then clean up |
| Absent | Matching original | Exact successor | Finish tombstone and journal cleanup |
| Absent | Absent | Exact successor | Finish directory/journal persistence checks and cleanup |
| Any foreign object or both leaf names present | Any | Any | Refuse without unlinking foreign data or discarding evidence |
| Absent | Absent | Exact old | Refuse: the protocol has no payload backup from which to restore the leaf |
| Any | Any | Other declaration | Refuse stale/foreign recovery without overwriting it |

Every accepted state must bind the same exact caller authority and validate
surviving graph edges, parent/sibling identities and accessible bytes. Repeated
recovery is idempotent. Unsupported/corrupt records, wrong grants or foreign
objects leave journal/artifacts unchanged. A read-only inspection command reports
pending state without cleanup or recovery. General garbage collection is separate.

## Acceptance before exposing mutation

- Behavioral failing tests first: sibling/ancestor preservation, selected-leaf
  policy, authority mismatch, base/non-leaf refusal, native pending recovery,
  aliases, artifact collisions, quotas and final cancellation before mutation.
- Cuts before/after every journal publication/sync, leaf rename/sync, manifest
  replacement/sync, tombstone unlink/sync and journal unlink/sync. Recover via
  the public explicit policy; compare complete surviving logical streams and
  manifest selection/edges. Check bytes/identity of parents and siblings.
- Wrong authority, replaced leaf/tombstone/manifest/directory, malformed journal,
  torn framing, stale expected declaration and attempted concurrent transactions
  refuse without deleting foreign objects. Exercise exact original/successor
  bytes, not only a phase flag, across every recovery decision above.
- Post-publication sync/cleanup failure retains updated live state and sufficient
  evidence. Repeated authorized recovery reaches the same final state. Process
  termination fixtures establish restart behavior, separate from actual device
  power-loss acceptance and native Windows/runtime gates.
- CLI defaults reject pending work; only explicit recovery plus exact authority
  can replay it. Progress/error output failure before commit cancels; output
  failure after commit cannot revoke deletion. Do not expose destructive CLI
  deletion until the protocol and recovery matrix pass.
- Run formatting, warnings-denied Linux/Windows cross-target Clippy and the locked
  serial host suite. Fuzz campaigns and replay remain paused by user instruction.

Implemented prerequisite: retained-lock manifest preparation/publication/sync
states, exercised by existing replacement and ownership/failure regressions.
This does not implement the image/manifest journal or persistent deletion.

Remaining implementation sequence: framing/authority and physical validation
budgets; pending-open guards; transaction
and public recovery together; full interruption/foreign-state matrix; then CLI
and platform/native acceptance. No step by itself completes persistent deletion.
