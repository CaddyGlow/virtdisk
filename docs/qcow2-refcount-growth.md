# QCOW2 refcount-block growth acceptance

The bounded 64 KiB-cluster, 16-bit-refcount writer already grows refcount
**blocks** when append allocation crosses their coverage boundary. Each block
contains 32,768 counters and covers 2 GiB of physical container offsets.
`tests/qcow2_refcount_growth.rs` exercises this branch through public APIs,
without allocating a 2 GiB payload or changing production code.

The fixture has 128 KiB logical capacity and one internal saved state sharing
the active L2 table and payload. After creating that state, the test extends the
container sparsely to `2 GiB - 64 KiB`; every added cluster remains unreferenced.
A two-byte partial write allocates its replacement payload in the last cluster
covered by the old block. Cloning the shared L2 then allocates at exactly 2 GiB,
requiring a second refcount block at `2 GiB + 64 KiB`.

Acceptance checks assert exact physical offsets, old payload/L2 counts dropping
from two to one, replacement payload/L2 counts of one, and the new refcount
block's own count of one. Both active and saved logical streams are compared
in full after reopening, and ownership validation runs against the resulting
container. Separate lifecycle cases verify that deletion releases the saved
original mappings, while revert releases the replaced active mappings and
preserves both restored and saved bytes. Both operations retain the new
refcount block's self-reference.

The independent QEMU case runs `qemu-img check` and converts the post-deletion
active image to raw for a full logical comparison. It is ignored by default
because it requires QEMU; it does not claim independent saved-state or
interruption-recovery acceptance.

```sh
nix develop --no-write-lock-file . --command cargo test --all-features --locked --test qcow2_refcount_growth
nix develop --no-write-lock-file . --command cargo test --all-features --locked --test qcow2_refcount_growth -- --include-ignored
```

On the development host, all three cases, including QEMU, passed in 8.00 seconds.
The redo journal still hashes the complete sparse container, so this is an
acceptance fixture rather than a cheap high-iteration fuzz seed. No production
defect was exposed by these cases.

Refcount **table** relocation is a separate remaining feature. One 64 KiB
table holds 8,192 block pointers, covering 16 TiB of physical offsets; that
boundary exceeds the current 33 GiB journal profile and bounded dense ownership
validation. Testing a relocation implementation honestly therefore requires
first expanding supported geometry or redesigning those bounds. These tests
establish block growth only. Fault-cut coverage at the 2 GiB boundary and
independent saved-state conversion remain follow-up work.
