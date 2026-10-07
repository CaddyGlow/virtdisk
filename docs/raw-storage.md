# Raw host allocation operations

`RawWriter::preallocate(offset, length)` requests allocation for an existing
bounded logical range. Linux uses `fallocate` with `KEEP_SIZE` under the retained
writer lock. Existing logical bytes and capacity are preserved, including data
inside the requested range. Valid zero-length requests do no host work.

Invalid ranges are rejected before mutation. Unsupported platforms and host
filesystems return `Unsupported`; errors do not trigger zero writes. Host I/O
errors propagate and may leave partially completed allocation. Call `flush`
to request durability. The host may allocate whole filesystem blocks outside
the precise byte boundaries; this API does not unshare reflinked blocks.

`RawWriter::discard` instead requests hole punching on Linux, retaining capacity
and making the range zero-readable. It requires an explicit policy to permit
zero-write fallback. Neither operation changes partition or filesystem layout.

`ImageWriter::preallocate` dispatches raw allocation and rejects other container
profiles. Container payload allocation is a separate operation: reserving host
bytes alone does not create a valid guest mapping or update ownership metadata.

CLI commands:

```text
virtdisk create OUTPUT FORMAT SIZE
virtdisk preallocate IMAGE raw OFFSET LENGTH
```

Creation uses existing per-family initial profiles, preserves exclusive access,
and never overwrites an existing destination. Raw creation may be sparse; QCOW2,
VDI and hosted VMDK initially allocate their payload mappings; VHDX creation is
sparse dynamic. Payload allocation is not a guarantee of physical host allocation.
Creation errors can leave a partial destination and do not sync its directory.

Tests measure allocated host blocks on the supported Linux test filesystem,
verify exact logical bytes, invalid-range behavior, retained locks, and native
reader reopening. Host measurements do not establish every filesystem's support.

Reference: [Linux fallocate contract](https://man7.org/linux/man-pages/man2/fallocate.2.html).
