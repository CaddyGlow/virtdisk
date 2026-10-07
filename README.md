# virtdisk

Read-only raw/QCOW2 virtual disk access with parser budgets. Partition tables
are parsed by `partmgr` and filesystem interpretation belongs to `disk-capture`.

`ReadAt` provides checked exact positional reads with a fixed logical length.
`RawDisk` retains a read-only regular-file handle. `DiskView` retains and bounds
its parent. `Qcow2::open_chain` opens only explicitly authorized backing paths.
Opening validates headers and read ranges; before capture, call
`validate_active_mapping_and_compressed_payloads` to check active allocation
ownership and every compressed descriptor in the chain.

## Supported profile

| Feature | Behavior |
| --- | --- |
| QCOW2 versions | v2 and v3 standard L1/L2 mapping |
| Clusters | 512 bytes through 2 MiB; allocated, unallocated and explicit-zero |
| Backing | Explicitly authorized raw/QCOW2 chains; canonical identity/cycle checks; maximum depth 32 |
| Compression | Pure Rust deflate and zstd; bounded decoding, window and total-work limits |
| Refcounts | All seven widths, exact reconstruction, copied-flag consistency, shared L2/data and leak checks |
| Physical ownership | Disjoint metadata; no data/metadata overlap; unused final L1 tail need not be physically padded |
| Partitions | Primary/backup GPT consistency and CRCs, unique identities and checked bounds; basic MBR; 512/4096-byte sectors |

Encryption, internal snapshots, persistent bitmaps, extended L2, external data,
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
cargo test --locked -p virtdisk
cargo test --locked -p virtdisk --test qcow2 -- --ignored
```

The ignored tests require `qemu-img` as an independent oracle. They cover v2/v3
randomized range reads, seven refcount widths, compressed codecs across four
cluster sizes, explicit-zero masking in backing chains, shared table/data
ownership, and QEMU's minimally stored L1 table. Host regression and bounded
fuzz tests cover malformed/truncated inputs. These checks do not establish
Windows installation correctness; see the
conversion evidence ledger in the historical windows-uup workspace.

## Releases

CI checks formatting, Clippy and tests on Linux and Windows. A matching version tag runs validation, verifies and publishes the crate to crates.io, then creates its GitHub Release with the crate and SHA-256 checksums.
CI also runs the ignored QCOW2 integration tests against QEMU as an independent oracle.
