# Windows-native VHDX acceptance harness

`tools/windows-acceptance` is an isolated Cargo workspace. It does not change
production or fuzz source. Its Windows executable opens only copied, manifest-
authorized VHDX trees through the native Microsoft virtual disk provider, attaches
read-only without drive letters, obtains the surfaced device from that handle,
and hashes every logical byte. It accepts no user-provided physical device path.
RAII closes handles and attempts detach on every error; explicit successful detach
is required for a passing gate.

Prepare the clean 512/4096-sector standalone and native partial-child artifacts:

```sh
nix develop --no-write-lock-file . --command cargo run --locked --manifest-path tools/windows-acceptance/Cargo.toml --bin prepare -- /data/cache/windows-vhdx-fixtures
env RUSTFLAGS='-C target-feature=+crt-static' nix develop --no-write-lock-file . --command cargo xwin build --locked --manifest-path tools/windows-acceptance/Cargo.toml --target x86_64-pc-windows-msvc --bin virtdisk-windows-acceptance
```

The Windows build statically links the C runtime so a clean Windows installation
does not need the Visual C++ redistributable. `RUSTFLAGS` applies only to this
command; it does not change the production or fuzz build configuration. Inspect
the resulting executable with `llvm-readobj --coff-imports` and confirm that
`VCRUNTIME140.dll` and `VCRUNTIME140_1.dll` are absent. Windows system libraries,
including `VirtDisk.dll`, remain native runtime dependencies.

The preparation command requires a new output directory. It emits checksummed
identity manifests and expected full logical hashes. The 4096-sector fixtures set
native logical sector metadata before creating their children; neither geometry
nor native acceptance is inferred from a file extension. Child fixtures cover
unaligned sector read/modify/write, private and inherited sectors sharing bitmaps,
whole-block zero masks followed by writes, and private zero sectors.

Copy the complete fixture directory to a local, uncompressed, unencrypted Windows
volume. Run the executable elevated with `SeManageVolumePrivilege` available:

```powershell
.\virtdisk-windows-acceptance.exe C:\fixtures\sector-512\base.vhdx.json
.\virtdisk-windows-acceptance.exe C:\fixtures\sector-512\child.vhdx.json
.\virtdisk-windows-acceptance.exe C:\fixtures\sector-4096\base.vhdx.json
.\virtdisk-windows-acceptance.exe C:\fixtures\sector-4096\child.vhdx.json
```

Each run creates a private temporary copy. Manifests are bounded to 64 KiB, 32
files, 16 authorized parents, and 64 GiB logical capacity. File content hashes and
lengths pin source identities. Symlinks and Windows reparse points are forbidden.
All supplied files must be the image or an explicitly authorized parent. Library
chain validation checks native linkage and opened parent identity first. The
native gate further permits only relative parent locators and linkage fields:
absolute, volume, unknown alternative locator keys and paths escaping the copied
tree are rejected, even if library path fallback could otherwise resolve them.

JSON stdout reports `passed`, `failed`, or `unfulfilled`; non-passes exit nonzero.
The native geometry must match the manifest, and the complete surfaced SHA256 must
match independently prepared expected content. Copied parents and clean images
must retain their before/after hashes. Attach privilege errors include actionable
instructions. The Linux executable validates fixtures but records `unfulfilled`;
cross-compilation never establishes Windows runtime acceptance.

## Recovery evidence is separate

The manifest `mode` distinguishes `clean`, `native_replay`, and `library_recovery`.
Dirty fixtures require independently captured or deterministic fault-produced
native logs, explicit expected logical hashes, and separately copied source trees.
Before native access a second disposable copy undergoes library recovery so its
final locator state is also checked; native replay never runs against the source.
`native_replay` leaves its tested copy dirty for Windows to open and recover;
that mode requests writable backing depth one and `VIRTUAL_DISK_ACCESS_ATTACH_RW`
for the copied leaf so the provider can replay metadata. Its parents remain
read-only. The handle also retains read-only attach permission, information and
detach rights; no create or metadata-operation access is requested. The surfaced
disk remains read-only because every mode passes
`ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY | ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER`
to attachment and opens the returned physical device with `GENERIC_READ` only.
This separates backing-store recovery permission from exposed device access,
following Microsoft's [open parameters](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/ns-virtdisk-open_virtual_disk_parameters)
and [attachment contract](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk).
`library_recovery` explicitly recovers its tested copy before Windows acceptance.
These modes report separate evidence and never substitute for one another.
The preparer emits separate `native_replay` and `library_recovery` directories,
each containing standalone and partial-child fixtures at both sector sizes. It
retains the actual writer-produced newest complete redo record and durable new
payload, restores every logged metadata target to its pretransaction bytes, and
activates both checksummed headers with that record's log GUID. This reconstructs
the durable-redo-before-metadata publication boundary; it is a deterministic
fixture, not a captured process interruption or physical power-loss test. Tests
require clean opens to reject each dirty leaf, recovered views to match every
byte of an independently constructed model, durable library recovery to permit a
clean reopen, and original fixture trees to remain unchanged. Expected logical
SHA256 values for clean and dirty fixtures come from the model, not the reader.
Windows-produced partial children and native power-loss runs remain necessary
gates. A read-only attachment does not imply the provider cannot
change recovery metadata; dirty image hashes are therefore recorded separately.

## Windows persistence work

Hosted sidecar transactions still reject Windows because durable journal name
publication/deletion currently requires Linux directory synchronization. Do not
replace this check with an ordinary rename or directory handle flush. A Windows
protocol must prove no-overwrite publication, opened identity, reparse rejection,
sharing/locking, content flush and durable completion through runtime interruption
tests. A persistent checksummed journal with prepared/committed generations could
avoid relying on deletion durability. Native VHDX redo is inside the image and
avoids that sidecar, but its flush, length and redundant-header ordering still
needs Windows runtime evidence.

Primary contracts: [OpenVirtualDisk](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-openvirtualdisk),
[open parameters](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/ns-virtdisk-open_virtual_disk_parameters),
[AttachVirtualDisk](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk),
[GetVirtualDiskPhysicalPath](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-getvirtualdiskphysicalpath),
[virtual disk information](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/ns-virtdisk-get_virtual_disk_info),
[FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers),
[MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).

## Recorded clean native acceptance

All four clean standalone/partial-child cases passed on Windows 11 Pro build
10.0.26200.8037. Each native open, attach and detach returned zero; all 4 MiB of
logical bytes matched the expected SHA256 at logical sector sizes 512 and 4096,
with physical sector size 4096. Copied leaf and parent file hashes were unchanged.
The [receipt](evidence/windows-vhdx-clean-20261007.json) records the individual
cases. The statically linked executable SHA256 was
`04dae3f5e6edc2900749b60365511d7ac07c327380c19ee9b49eb3f7f659ef68`.
This clean receipt does not establish dirty replay or power-loss behavior.

## Recorded native dirty replay

All eight deterministic dirty cases passed on the same Windows build: native
redo replay and library recovery followed by native attachment, standalone and
partial child, with both logical sector sizes. Every complete 4 MiB surfaced hash
matched the independent model and every copied parent hash remained unchanged.
The [dirty receipt](evidence/windows-vhdx-dirty-20261007.json) records the native
results and before/after image hashes. Native replay uses executable SHA256
`2891cbdf1e2150e4dac40e8ce843f2d768665a4d075ca3e6b4709d8306c4d203`.
These reconstructed log cuts do not establish physical power-loss persistence.

## Native Windows partial-child producer

Build the isolated producer with the same static runtime configuration:

```sh
env RUSTFLAGS='-C target-feature=+crt-static' nix develop --no-write-lock-file . --command cargo xwin build --locked --manifest-path tools/windows-acceptance/Cargo.toml --target x86_64-pc-windows-msvc --bin produce
```

On Windows, use an elevated process and a new output directory for each case:

```powershell
.\produce.exe C:\fixtures\sector-512\base.vhdx.json C:\native-child-512
.\produce.exe C:\fixtures\sector-4096\base.vhdx.json C:\native-child-4096
```

The producer accepts only a checksummed clean standalone 4 MiB constant-seven
parent. It validates every logical byte before native creation, copies that parent
to its new output directory, and calls native `CreateVirtualDisk` version two to
create a differencing child. It opens only that new child with writable backing
depth one, verifies native capacity and logical/physical geometry before writes,
and attaches without drive letters. The device path comes exclusively from that
child's native handle; the command accepts no physical device path. It writes
one logical sector at offsets 8192, 1056768 and 3153920, filled with bytes 11, 13
and 17 respectively. All other bytes must retain their inherited value seven.
Device flush and explicit detach are required, followed by read-only native
attachment and a full independent-model SHA256 check. Parent file hashes and
original source identities must remain unchanged.

The output retains the native provider's child bytes and locator metadata.
Download both `base.vhdx` and `child.vhdx` for explicit-parent library validation;
do not edit native locators to fit the stricter read-only attachment harness.
Both canonical native producer cases passed on Windows 11 Pro build
10.0.26200.8037. The downloaded, unmodified children contain
`PARTIALLY_PRESENT` payload BAT entries (state seven) and a fully present sector
bitmap (state six). The library read every logical byte of each child against
the independent model at both logical sector sizes; leaf and parent hashes stayed
unchanged, and opening without the explicitly authorized parent failed. The
[producer receipt](evidence/windows-vhdx-produced-20261007.json) and compressed
native fixtures in `tests/fixtures/vhdx/windows-11-26200` preserve that evidence.
The test is a reproducible native-provider interoperability check, not a Windows
servicing, capture or power-loss correctness claim.

Native Windows production can emit an ordinary drive-rooted
`absolute_win32_path` such as `C:\directory\base.vhdx`, although the published
VHDX locator examples use extended-length paths. The library recognizes that
native form alongside the existing extended namespace, while still requiring
explicit parent authorization and matching opened file identity. It rejects relative drive forms, device namespaces and alternate data streams.
Ordinary and extended Windows absolute namespaces resolve only on Windows; on
other hosts they report `Unsupported` if no usable earlier relative locator
resolves the explicitly authorized parent. Parent locator values remain unchanged in the native artifact.

## Rust writer operations on Windows

The isolated `mutate` executable exercises Rust writer behavior on a new owned
copy of each known clean library fixture, followed by native read-only full-model
hash validation. Build it with command-scoped static CRT flags:

```sh
env RUSTFLAGS='-C target-feature=+crt-static' nix develop --no-write-lock-file . --command cargo xwin build --locked --manifest-path tools/windows-acceptance/Cargo.toml --target x86_64-pc-windows-msvc --bin mutate
```

```powershell
.\mutate.exe C:\fixtures\sector-512\child.vhdx.json C:\rust-mutation-512
.\mutate.exe C:\fixtures\sector-4096\child.vhdx.json C:\rust-mutation-4096
```

Run separate new directories for standalone fixtures as well. The workload checks
competing writer lock refusal, changes seven bytes across a logical-sector boundary,
writes inherited bytes in two other sectors, zeroes eight bytes, flushes, releases
the writer, and reopens the complete image against an independent byte model.
It additionally creates a new sparse standalone 512-sector image and a native
relative-parent overlay at the input sector size, verifies initial zeros where
applicable, and applies the same bounded write workload. Native attachment remains
read-only and requires explicit detach; source identities and copied parents must
remain unchanged. Known relative locators are checked before native API access.
Artifacts remain in the new output directory for inspection after a failure.

Native Windows discard and capacity mutation are currently guarded by the
production implementation. The workload requires `Unsupported` and unchanged
container hashes for those operations; these refusals are recorded separately
from successful writes. A successful steady-state mutation receipt does not prove
process interruption or power-loss ordering. All four actual Windows runs passed on Windows 11 Pro build 10.0.26200.8037:
standalone and child inputs at logical sector sizes 512 and 4096, each with three
cases. The twelve cases verified Rust mutation, creation or overlay creation,
exclusive writer locking, flush/drop/reopen, and native read-only full 4 MiB
SHA256 against the independent model. Every native readout left its clean leaf
unchanged; copied parents and original sources remained unchanged. Windows native
discard and resize refused unchanged as expected. This receipt fulfills bounded
steady-state writer interoperability. Process-interruption evidence is recorded
separately below; physical power-loss persistence remains unfulfilled.

The runtime receipt is
`/data/cache/virtdisk-native-windows/receipts/20261007T175334Z.json`; executable
SHA256 is
`3e18ce90d513d78308a6d404e013a51c7a735311a0cfdf0bb1b8598079794d05`.
Host byte-model tests and cross-compilation alone do not establish these runtime
results.

## Recorded instrumented process-termination acceptance

The isolated `process-kill` controller and worker implement the following bounded
design. Three host tests cover bounded x64 PE imports, the fixed acknowledgement
protocol, controller sequencing and independent old/new models. Actual Windows
11 Pro build 10.0.26200.8037 execution passed both logical sector sizes: 100 cases,
comprising 84 terminated workers and 16 successful controls/traces. Every complete
4 MiB result matched an independently constructed old or new model through both
library recovery followed by native readout and native provider replay. Native
open, attach and detach returned zero throughout; recovered clean leaves and
copied parents remained unchanged during readout. Every terminated worker exited
with `0x56444355` at its acknowledged cut. This establishes this instrumented
process-termination gate; physical power-loss persistence remains unfulfilled.

Use a controller and a Windows worker operating only on a new, manifest-authorized
copy. After fixture creation, validation and opening the writer, the worker intercepts its own executable import-address-table entry for `FlushFileBuffers`.
First verify that the built executable imports that API and that the production
`File::sync_all` call actually reaches the intercepted entry. Do not assume this
from a symbol name or a compiler version. If the import is absent, forwarded
through an unobserved entry, or bypassed, stop with an unfulfilled gate rather
than silently using timing-based termination.

The wrapper must use the exact Windows `extern "system"` ABI and signature, retain
a separately verified original API pointer, and forward to that real API exactly
once without recursively calling the patched slot. Interception applies only to
the worker's executable import slot; it changes neither production source nor
system DLL instructions. Bound and validate PE import-table traversal and restore
page protections after installing the pointer. Reject unsupported executable
architectures or layouts. The wrapper must identify the already authorized leaf
by its opened file identity, not by a user-supplied device path or a filename
suffix. Calls for other handles pass through without acknowledgements or pauses.

Only after the real API returns `TRUE` may the wrapper send a fixed-size,
allocation-free acknowledgement containing the cut number and owned file
identity through pre-established controller IPC. No allocations, formatted
logging, file flushes or initialization are permitted in that acknowledgement
path. A failed flush must retain its return value and Windows error code and must
never be counted as a durable cut. After acknowledgement the worker blocks on a
controlled gate before returning to production code, so the next mutation cannot
run before the controller chooses continuation or termination. IPC errors,
identity disagreement and unexpected flush counts fail the case; they are not
successful interruption evidence.

Run the ordinary workload without interception as a negative control, then run
an intercepted trace with every gate released. Both must complete against the
same independent full-byte model and native read-only hash oracle. The trace
establishes the observed successful flush count for that artifact and workload.
For each cut, start a fresh worker and copy, wait for its acknowledgement, and
terminate the blocked process through the controller. Record its actual process
identifier, acknowledged identity and cut, successful termination request,
terminal process status and executable hash. A worker returning an injected
error, exiting normally, running Rust destructors, or merely reconstructing a
log image does not satisfy this terminated-worker requirement.

The implemented workload uses one unaligned eight-byte write that fits within a
single logical sector, at both 512 and 4096 sector sizes, on sparse standalone
and relative-parent child images. Check the pre-write cut and every observed
successful flush boundary. A separate retained-epoch case first completes a
write, preserve the live writer, reset observation, and interrupt a second write
to a different inherited sector in the same block. Old and new full-byte models
must be constructed before the experiment; interruption permits only the
specified model outcomes, never an arbitrary reader-produced expected hash.

After confirmed termination, preserve and checksum the untouched interrupted
artifact before any recovery. Use separate copies for library recovery followed
by native read-only attachment, and for native provider replay followed by a
read-only surfaced device. Require complete model verification, mandatory detach,
source and parent immutability, and evidence of released writer locks. Preserve
raw header, log and mapping observations so a numbered successful flush can be
associated with the actual resulting state. These are instrumented real Windows process-termination tests, distinct from
simulated I/O failures and reconstructed dirty fixtures. Physical power-loss and
host-storage durability requirements remain open.

Build and run the instrumented harness after reviewing its worker instrumentation:

```sh
env RUSTFLAGS='-C target-feature=+crt-static' nix develop --no-write-lock-file . --command cargo xwin build --locked --manifest-path tools/windows-acceptance/Cargo.toml --target x86_64-pc-windows-msvc --bin process-kill
```

```powershell
.\process-kill.exe C:\fixtures\sector-512\base.vhdx.json C:\process-cuts-512
.\process-kill.exe C:\fixtures\sector-4096\base.vhdx.json C:\process-cuts-4096
```

The command accepts only a clean, pinned standalone constant-seven 4 MiB source.
It prepares sparse and relative-parent child workloads in new owned directories;
4096-sector sparse fixture geometry is set and validated before worker launch.
It runs a no-interception control and a released-gate trace before any cut in each
fresh or retained workload. Trace acknowledgement count must be positive and at
most 128. Each cut uses a new worker and copy. The pre-write cut uses the ready
acknowledgement; other cuts use successful real flush acknowledgements. Explicit
`TerminateProcess` and terminal exit code `0x56444355` are required.

`process.json` records termination and the untouched leaf checksum before
recovery verification. The raw interrupted leaf and authorized parent remain in
that case directory. `receipt.json` adds paired recovery and full native hash
verification after success, while `partial-receipt.json` retains completed cases
if a later case fails. Neither simulated failures nor normal worker exits count
as killed-process cases. The tested static executable SHA256 is
`42113ceeb9577adf319bf5e4c3eba10df74328eb39414620eaed33af1f45fbb3`.
The runtime receipt is
`/data/cache/virtdisk-native-windows/receipts/20261007T190438Z.json`.

Each sector size produced 50 cases and 42 actual terminations. Successful flush
counts were identical for both sector sizes:

| Workload | Successful observed flushes |
| --- | ---: |
| Sparse, fresh epoch | 10 |
| Sparse, retained writer | 1 |
| Child, fresh epoch | 18 |
| Child, retained epoch | 9 |

The retained sparse case modifies already private payload and observes its final
flush; it does not imply a fresh logged mapping transaction. Child cases cover
inherited sectors and retained native redo. The results establish only the
specified single-sector mutation workloads, acknowledged flush boundaries and
instrumented process termination. They do not establish arbitrary multiblock
write atomicity, storage-device power-loss durability or Windows servicing and
capture correctness.
