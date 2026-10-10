# Windows VM benchmark results — 2026-10-10

All eight conversion cases completed with five measured samples per provider and one warm-up. All 110 measured outputs and 22 warm-up outputs passed independent QEMU raw decoding, exact length and SHA-256 verification. The native Rust harness completed all 20 format/workload groups with seven measured samples each and payload verification.

## Environment

Windows 11 Pro, build 26200, in a disposable `win11-dev` QEMU/KVM guest; four vCPUs on an Intel i7-13700K, 6 GiB RAM, VirtIO disk, NTFS C: on a QCOW2 overlay. The shared host was not idle (recorded load averages: 9.02 / 24.53 / 25.22). The immutable base was retained.

Installed tools: Oracle VirtualBox 7.2.20r175154, QEMU 11.1.0 (`v11.1.0-12130-ge470268ff4`), Hyper-V PowerShell module 2.0.0.0 and its running VM management service, Python 3.13.9. The virtdisk CLI and Rust harness were cross-compiled with the repository development shell in release/bench mode for x86_64-pc-windows-msvc, then executed natively inside the guest.

VirtualBox and QEMU installer checksums matched their published manifests. Installer download URLs and hashes, executable hashes, library/benchmark source hashes, guest hardware and feature inventory are retained in the evidence directory.

## Conversion throughput

Median logical MiB/s; 256 MiB images, one warm-up, five measured samples, seeded randomized provider order. Sparse data has one nonzero MiB per sixteen. QCOW2, VDI and VMDK use raw input; VHDX uses the same prebuilt VHDX input for all providers.

| Output | Data | virtdisk | QEMU | VirtualBox | Hyper-V |
| --- | --- | ---: | ---: | ---: | ---: |
| QCOW2 | dense | 531.2 | 1,365.1 | — | — |
| VDI | dense | 361.2 | 926.2 | 476.4 | — |
| VMDK | dense | 467.5 | 157.9 | 525.0 | — |
| VHDX | dense | 439.1 | 980.4 | — | 110.7 |
| QCOW2 | sparse | 511.5 | 1,821.2 | — | — |
| VDI | sparse | 1,059.5 | 2,397.5 | 2,140.6 | — |
| VMDK | sparse | 1,426.8 | 1,309.8 | 2,191.8 | — |
| VHDX | sparse | 1,611.5 | 2,208.3 | — | 107.6 |

These are end-to-end default command comparisons. virtdisk includes staged-output verification and publication; Hyper-V includes PowerShell startup/module loading. Each provider retains its default allocation and flush policies. The extra independent verification after each command is excluded from timing. The data fit in guest RAM and caches were retained. These results do not establish a general backend or physical-storage ranking.

## Sparse output container lengths

Median file length in MiB, not host allocated storage. Different defaults produce materially different containers even when logical bytes match.

| Output | virtdisk | QEMU | VirtualBox | Hyper-V |
| --- | ---: | ---: | ---: | ---: |
| QCOW2 | 256.31 | 16.31 | — | — |
| VDI | 16.00 | 16.00 | 18.00 | — |
| VMDK | 16.06 | 16.06 | 16.06 | — |
| VHDX | 20.00 | 136.00 | — | 260.00 |

For example, virtdisk default QCOW2 conversion retained approximately 256 MiB of container length where QEMU produced approximately 16 MiB; virtdisk sparse VHDX produced approximately 20 MiB where QEMU produced 136 MiB and Hyper-V 260 MiB. Use the throughput and size tables together.

## Native Rust I/O

Median MiB/s, 64 MiB images, one warm-up and seven measured samples. Sequential operations use 1 MiB requests; random operations use a fixed permutation of 4 KiB blocks. Writes overwrite existing allocation and include one flush per sample. Opening, initial population and payload verification are excluded.

| Format | Sequential read | Random read | Sequential write + flush | Random write + flush |
| --- | ---: | ---: | ---: | ---: |
| Raw | 5,736.9 | 908.8 | 1,169.5 | 542.6 |
| Qcow2 | 3,540.8 | 870.2 | 1,084.4 | 497.8 |
| Vdi | 5,303.5 | 713.9 | 1,074.5 | 425.1 |
| Vmdk | 4,595.6 | 874.2 | 821.7 | 420.1 |
| Vhdx | 4,401.1 | 857.0 | 769.7 | 421.0 |

These are warm-cache buffered library operations, with filesystem, guest and host caches in the path. No allocation, snapshot COW, cold-cache, per-request durability, guest filesystem or VM execution throughput was measured.

## Evidence and reproduction

[Raw JSON samples, Rust CSV, environment, source hashes and runner](evidence/provider-benchmarks-windows-20261010/), with file hashes in `SHA256SUMS.json`. The PowerShell runner records the exact eight configurations; each JSON report retains individual measured commands, durations and container sizes. See [benchmark usage and methodology](benchmarks.md).

The installed guest state is retained at `/data/cache/vm-state/virtdisk-perf-1010`; installers, executables and installation logs are retained at `/data/cache/virtdisk-provider-vm-20261010`. From `/data/cache`, resume with `vm-run --base win11-dev virtdisk-perf-1010`. Use a new result directory when repeating the saved runner, since reports reject overwriting existing files.
