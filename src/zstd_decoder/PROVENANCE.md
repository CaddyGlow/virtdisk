# Private bounded Zstandard decoder

Derived from the MIT-licensed `ms-compress-ruzstd` 0.9.1 source cached from
crates.io (`https://github.com/CaddyGlow/zstd-rs`). The complete original MIT
license is retained in `LICENSE`. This decoder is bundled inside virtdisk and
has no independently published API or dependency on an external decoder crate.

Changes:

- Remove encoders and standalone crate setup; adapt internal module paths.
- Keep checksum verification enabled in both feature configurations.
- Reject block compressed/decompressed sizes above min(window, 128 KiB).
- Reject literal regenerated sizes above that bound before materialization.
- Reject sequence counts above block_size / 3 before sequence reservation.
- Check FSE symbol limits before every probability-table push/zero-run resize.
- Check regenerated literal counts before each Huffman literal push.
- Preflight all sequence literal consumption and cumulative match output before
  any output mutation or expansion; reject output above the block bound.
- Disable upstream standalone tests/examples; dedicated virtdisk fixtures cover
  valid output, checksums, truncation, oversized literals, sequence counts,
  expansion and reservation release. Private decoder unit tests target preflight.

QCOW2 reserves input + output + 2 * window + 16 * max_block + 64 KiB before
creating the decoder. Geometric capacity growth of history, blocks, literals,
sequences and the temporary raw-block buffer is covered conservatively. The
64 KiB fixed allowance covers bounded Huffman/FSE tables and decoder state.
Dictionary installation is unused. Internal allocation remains infallible;
this code does not claim recoverable allocator failure for those allocations.
The existing fallibly allocated QCOW2 input and output buffers retain typed
allocation failure behavior.
