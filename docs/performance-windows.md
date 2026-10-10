# Windows performance candidate runtime acceptance

The streaming candidate passed native consumer checks in the uniquely owned
Windows 11 25H2 guest `virtdisk-performance-acceptance-20261010`. The current
MSVC CLI produced dense and sparse VDI/VHDX images with a 4 MiB working-memory
limit and explicit streaming allocation. Publication retained the library's
exact whole-image verification.

VirtualBox independently cloned each VDI to raw; both raw SHA-256 hashes matched
the source exactly. Windows DiskPart independently attached each VHDX read-only;
reading the entire virtual capacity through `PhysicalDrive` produced the same
source hashes. Each VHDX was detached in a `finally` block. Fixtures exercise a
512-byte final partial allocation unit. The host fixture generator also asserts
zero `AllocationRead` bytes, excluding two-pass fallback from its candidate
fixtures.

The retained [result](evidence/windows-performance-20261011/result.json) and
[native output](evidence/windows-performance-20261011/native-result.json) identify
the binary, inputs, outputs, consumer versions and four successful checks. The
[verification script](evidence/windows-performance-20261011/verify.ps1) creates a
fresh private output directory for every run, avoiding VirtualBox registry UUID
collisions when regenerated containers receive new identities.

The guest required OOBE recovery and the VC runtime before the candidate could
run. Hyper-V management features were installed and rebooted, but `Mount-VHD`
could not access the nested guest's expected WMI platform; DiskPart exercised the
Windows virtual disk stack directly. This is format/runtime correctness evidence
for these two profiles. It does not establish Windows storage speedups, crash
recovery, physical power-loss behavior, or the full M4–M7 performance exit gates.
