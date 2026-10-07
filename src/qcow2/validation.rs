//! Strict active-map structural checks; never repair or change the image.
use super::{COPIED, OFFSET_MASK, Qcow2, be64, invalid, unsupported};
use std::io;

const WORK_LIMIT: u64 = 16 * 1024 * 1024;
const METADATA_LIMIT: usize = 1024 * 1024;
const HOST_CLUSTER_LIMIT: u64 = 16 * 1024 * 1024;
const COMPRESSED_OUTPUT_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

/// Evidence returned by strict active-map validation across an opened chain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Qcow2Validation {
    /// Number of QCOW2 containers checked; raw backing files are excluded.
    pub containers: u64,
    /// Bounded units of metadata/table/reference work performed.
    pub work_items: u64,
    /// Disjoint metadata extents retained after deduplication.
    pub metadata_extents: u64,
    /// Nonempty allocated, preallocated-zero, or compressed data descriptors.
    pub data_descriptors: u64,
    /// Compressed descriptors included in `data_descriptors`.
    pub compressed_clusters: u64,
    /// Distinct physical clusters with a reconstructed nonzero owner count.
    pub referenced_clusters: u64,
    /// On-disk refcount counters compared, including unused/out-of-file counters.
    pub refcount_entries_checked: u64,
    /// Compressed descriptors successfully decoded by the optional payload audit.
    pub compressed_payloads_verified: u64,
    /// Logical bytes decoded by the optional compressed-payload audit.
    pub compressed_bytes_verified: u64,
}

struct Budget<'a> {
    stats: Qcow2Validation,
    cancelled: &'a mut dyn FnMut() -> bool,
    audit_compressed: bool,
    parser: Option<crate::ReadBudget>,
}
impl Budget<'_> {
    fn step(&mut self) -> io::Result<()> {
        if let Some(parser) = &self.parser {
            parser.work(1)?;
        }
        if self.stats.work_items >= WORK_LIMIT {
            return Err(unsupported(
                "QCOW2 validation exceeds 16 million work-item limit",
            ));
        }
        if self.stats.work_items.is_multiple_of(1024) && (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "QCOW2 validation cancelled",
            ));
        }
        self.stats.work_items += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Extent {
    start: u64,
    end: u64,
    kind: u8,
}
fn add_extent(
    extents: &mut Vec<Extent>,
    start: u64,
    length: u64,
    kind: u8,
    budget: &Budget<'_>,
) -> io::Result<()> {
    if let Some(parser) = &budget.parser {
        parser.metadata(std::mem::size_of::<Extent>() as u64)?;
    }
    if extents.len() >= METADATA_LIMIT {
        return Err(unsupported(
            "QCOW2 validation exceeds metadata-extent limit",
        ));
    }
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid("QCOW2 metadata extent overflow"))?;
    extents.push(Extent { start, end, kind });
    Ok(())
}

struct Refcounts<'a> {
    disk: &'a Qcow2,
    table: &'a [u8],
    block_offset: u64,
    block: Vec<u8>,
}
impl Refcounts<'_> {
    fn value(&mut self, offset: u64) -> io::Result<u64> {
        let width = 1u64 << self.disk.refcount_order;
        let entries = self.disk.cluster_size * 8 / width;
        let index = offset / self.disk.cluster_size;
        let table_index = usize::try_from(index / entries)
            .map_err(|_| invalid("QCOW2 refcount index overflow"))?;
        let table_entry = self
            .table
            .get(table_index.saturating_mul(8)..table_index.saturating_mul(8).saturating_add(8))
            .ok_or_else(|| invalid("QCOW2 referenced cluster is outside refcount table"))?;
        let block_offset = be64(table_entry);
        if block_offset == 0 {
            return Err(invalid("QCOW2 referenced cluster has no refcount block"));
        }
        if block_offset != self.block_offset {
            self.disk
                .source
                .read_exact_at(block_offset, &mut self.block)?;
            self.block_offset = block_offset;
        }
        Ok(refcount_value(&self.block, index % entries, width))
    }
    fn nonzero(&mut self, offset: u64) -> io::Result<()> {
        if self.value(offset)? == 0 {
            return Err(invalid("QCOW2 referenced cluster has zero refcount"));
        }
        Ok(())
    }
}

fn refcount_value(block: &[u8], index: u64, width: u64) -> u64 {
    let bit = index * width;
    if width < 8 {
        // QEMU's sub-byte counters start at the least significant bit.
        (u64::from(block[(bit / 8) as usize]) >> (bit % 8)) & ((1 << width) - 1)
    } else {
        let start = (bit / 8) as usize;
        block[start..start + (width / 8) as usize]
            .iter()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte))
    }
}

fn reference(expected: &mut [u32], cluster: u64, budget: &mut Budget<'_>) -> io::Result<()> {
    let count = expected
        .get_mut(cluster as usize)
        .ok_or_else(|| invalid("QCOW2 reference exceeds physical image"))?;
    if *count == 0 {
        budget.stats.referenced_clusters += 1;
    }
    *count = count
        .checked_add(1)
        .ok_or_else(|| invalid("QCOW2 reconstructed refcount overflow"))?;
    Ok(())
}

impl Qcow2 {
    /// Validate active mapping structure and every retained QCOW2 backing container.
    ///
    /// Checks disjoint metadata extents, data/metadata separation, descriptor
    /// validity, exact reconstructed refcounts, copied-bit ownership, and leaked
    /// allocations across all represented refcount blocks.
    /// Internal snapshots and extensions with unaudited external metadata
    /// (including persistent bitmaps) are rejected.
    /// Payloads are not decompressed here; read/checksum failures still propagate
    /// when consumed. Host-cluster accounting is capped at 16 million entries
    /// (64 MiB of reconstructed counts) per container.
    /// Work is capped at 16 million visited table entries across the chain, one
    /// million metadata extents, and an 8 MiB refcount table per container.
    pub fn validate_active_mapping(&self) -> io::Result<Qcow2Validation> {
        self.validate_active_mapping_with_cancel(|| false)
    }

    /// Validate with a cancellation predicate checked at least every 1024 entries.
    /// Cancellation returns [`io::ErrorKind::Interrupted`] without modifying sources.
    pub fn validate_active_mapping_with_cancel(
        &self,
        mut cancelled: impl FnMut() -> bool,
    ) -> io::Result<Qcow2Validation> {
        let mut budget = Budget {
            stats: Qcow2Validation::default(),
            cancelled: &mut cancelled,
            audit_compressed: false,
            parser: self.source.budget(),
        };
        self.validate_inner(&mut budget)
            .map_err(|e| self.source.context().error("validate QCOW2 ownership", e))?;
        Ok(budget.stats)
    }

    /// Validate ownership and decode every allocated compressed descriptor.
    ///
    /// Includes descriptors outside captured partitions/files and all QCOW2
    /// parents. Uses bounded cluster buffers; decoded output is capped at 64 GiB
    /// across the chain. Uncompressed descriptors have no intrinsic checksum,
    /// and this method does not read their payloads. Deflate has no checksum;
    /// valid decoding establishes structure/length rather than original content.
    pub fn validate_active_mapping_and_compressed_payloads(&self) -> io::Result<Qcow2Validation> {
        self.validate_active_mapping_and_compressed_payloads_with_cancel(|| false)
    }

    /// Validate ownership and compressed payloads with periodic cancellation.
    pub fn validate_active_mapping_and_compressed_payloads_with_cancel(
        &self,
        mut cancelled: impl FnMut() -> bool,
    ) -> io::Result<Qcow2Validation> {
        let mut budget = Budget {
            stats: Qcow2Validation::default(),
            cancelled: &mut cancelled,
            audit_compressed: true,
            parser: self.source.budget(),
        };
        self.validate_inner(&mut budget)
            .map_err(|e| self.source.context().error("validate QCOW2 ownership", e))?;
        Ok(budget.stats)
    }

    fn validate_inner(&self, budget: &mut Budget<'_>) -> io::Result<()> {
        budget.step()?;
        budget.stats.containers += 1;
        if self.snapshots != 0 {
            return Err(unsupported(
                "QCOW2 strict validation does not support internal snapshots",
            ));
        }
        if self.extra_metadata {
            return Err(unsupported(
                "QCOW2 strict validation does not support additional metadata extensions",
            ));
        }
        let refcount_length = self.refcount_clusters * self.cluster_size;
        if refcount_length > 8 * 1024 * 1024 {
            return Err(unsupported(
                "QCOW2 refcount table exceeds 8 MiB validation limit",
            ));
        }
        let host_clusters = self.source.len().div_ceil(self.cluster_size);
        if host_clusters > HOST_CLUSTER_LIMIT {
            return Err(unsupported(
                "QCOW2 image exceeds 16 million physical-cluster accounting limit",
            ));
        }
        if let Some(parser) = &budget.parser {
            parser.metadata(host_clusters * 4 + refcount_length)?;
        }
        let mut expected = vec![0u32; host_clusters as usize];
        let mut table = vec![0; refcount_length as usize];
        self.source
            .read_exact_at(self.refcount_offset, &mut table)?;
        let mut metadata = Vec::new();
        add_extent(&mut metadata, 0, self.cluster_size, 0, budget)?;
        add_extent(
            &mut metadata,
            self.refcount_offset,
            refcount_length,
            1,
            budget,
        )?;
        if self.l1_size != 0 {
            let length = (self.l1_size * 8).div_ceil(self.cluster_size) * self.cluster_size;
            // QEMU writes only the actual L1 entries when this allocation ends
            // the file. Its unused cluster tail need not be physically present;
            // ownership and overlap accounting still reserve the whole cluster.
            Self::cluster_range(
                &*self.source,
                self.l1_offset,
                self.l1_size * 8,
                self.cluster_size,
            )?;
            add_extent(&mut metadata, self.l1_offset, length, 2, budget)?;
        }
        let mut refcount_blocks = std::collections::HashSet::new();
        for entry in table.chunks_exact(8) {
            budget.step()?;
            let offset = be64(entry);
            if offset != 0 {
                if !refcount_blocks.insert(offset) {
                    return Err(invalid("QCOW2 refcount table aliases a block"));
                }
                Self::cluster_range(&*self.source, offset, self.cluster_size, self.cluster_size)?;
                add_extent(&mut metadata, offset, self.cluster_size, 3, budget)?;
            }
        }
        let mut l2_tables = Vec::new();
        for index in 0..self.l1_size {
            budget.step()?;
            let entry = self.entry(self.l1_offset + index * 8)?;
            if entry & !(OFFSET_MASK | COPIED) != 0 {
                return Err(invalid("QCOW2 L1 reserved bits are set"));
            }
            let offset = entry & OFFSET_MASK;
            if offset == 0 {
                if entry != 0 {
                    return Err(invalid("QCOW2 unallocated L1 entry has copied flag"));
                }
                continue;
            }
            Self::cluster_range(&*self.source, offset, self.cluster_size, self.cluster_size)?;
            add_extent(&mut metadata, offset, self.cluster_size, 4, budget)?;
            if let Some(parser) = &budget.parser {
                parser.metadata(32)?;
            }
            l2_tables.push((index, offset, entry & COPIED != 0));
        }
        // Count every L1 reference to shared L2 metadata before deduplication.
        for extent in &metadata {
            for offset in (extent.start..extent.end).step_by(self.cluster_size as usize) {
                budget.step()?;
                reference(&mut expected, offset / self.cluster_size, budget)?;
            }
        }
        metadata.sort_unstable();
        metadata.dedup();
        budget.stats.metadata_extents += metadata.len() as u64;
        for pair in metadata.windows(2) {
            if pair[0].end > pair[1].start {
                return Err(invalid("QCOW2 metadata extents overlap"));
            }
        }
        let _cache_reservation = budget
            .parser
            .as_ref()
            .map(|p| p.cache(self.cluster_size))
            .transpose()?;
        let mut refs = Refcounts {
            disk: self,
            table: &table,
            block_offset: 0,
            block: vec![0; self.cluster_size as usize],
        };
        for extent in &metadata {
            for offset in (extent.start..extent.end).step_by(self.cluster_size as usize) {
                budget.step()?;
                refs.nonzero(offset)?;
            }
        }
        if let Some(parser) = &budget.parser {
            parser.metadata(self.cluster_size)?;
        }
        let mut l2_bytes = vec![0; self.cluster_size as usize];
        let entries = self.cluster_size / 8;
        for (l1_index, l2_offset, copied) in l2_tables {
            if (refs.value(l2_offset)? == 1) != copied {
                return Err(invalid("QCOW2 L1 copied bit disagrees with ownership"));
            }
            self.source.read_exact_at(l2_offset, &mut l2_bytes)?;
            for (index, entry) in l2_bytes.chunks_exact(8).enumerate() {
                budget.step()?;
                let raw = be64(entry);
                if raw == 0 {
                    continue;
                }
                let guest = (l1_index * entries + index as u64) * self.cluster_size;
                if guest >= self.size {
                    return Err(invalid("QCOW2 allocated L2 entry lies beyond virtual size"));
                }
                let mapping = self.mapping_descriptor(raw)?;
                let (start, length) = match mapping {
                    super::Mapping::Allocated(start) => (start, self.cluster_size),
                    super::Mapping::Compressed(start, length) => (start, length as u64),
                    super::Mapping::Zero => {
                        let host = raw & OFFSET_MASK;
                        if host == 0 {
                            continue;
                        }
                        (host, self.cluster_size)
                    }
                    super::Mapping::Backing => continue,
                };
                if !matches!(mapping, super::Mapping::Compressed(_, _))
                    && (refs.value(start)? == 1) != (raw & COPIED != 0)
                {
                    return Err(invalid("QCOW2 L2 copied bit disagrees with ownership"));
                }
                budget.stats.data_descriptors += 1;
                if let super::Mapping::Compressed(start, length) = mapping {
                    budget.stats.compressed_clusters += 1;
                    if budget.audit_compressed {
                        if budget.stats.compressed_bytes_verified + self.cluster_size
                            > COMPRESSED_OUTPUT_LIMIT
                        {
                            return Err(unsupported(
                                "QCOW2 compressed audit exceeds 64 GiB decoded-output limit",
                            ));
                        }
                        self.decompress(start, length)?;
                        budget.stats.compressed_payloads_verified += 1;
                        budget.stats.compressed_bytes_verified += self.cluster_size;
                    }
                }
                let end = start
                    .checked_add(length)
                    .ok_or_else(|| invalid("QCOW2 data extent overflow"))?;
                let before_end = metadata.partition_point(|extent| extent.start < end);
                if before_end != 0 && metadata[before_end - 1].end > start {
                    return Err(invalid("QCOW2 data overlaps image metadata"));
                }
                for cluster in (start / self.cluster_size)..=(end - 1) / self.cluster_size {
                    budget.step()?;
                    refs.nonzero(cluster * self.cluster_size)?;
                    reference(&mut expected, cluster, budget)?;
                }
            }
        }
        let width = 1u64 << self.refcount_order;
        let counters = self.cluster_size * 8 / width;
        if let Some(parser) = &budget.parser {
            parser.metadata(self.cluster_size)?;
        }
        let mut refcount_block = vec![0; self.cluster_size as usize];
        for (table_index, entry) in table.chunks_exact(8).enumerate() {
            let offset = be64(entry);
            if offset == 0 {
                continue;
            }
            self.source.read_exact_at(offset, &mut refcount_block)?;
            for local in 0..counters {
                budget.step()?;
                budget.stats.refcount_entries_checked += 1;
                let cluster = table_index as u64 * counters + local;
                let observed = refcount_value(&refcount_block, local, width);
                let reconstructed = expected.get(cluster as usize).copied().unwrap_or(0);
                if observed != u64::from(reconstructed) {
                    return Err(invalid(
                        "QCOW2 exact refcount mismatch or leaked allocation",
                    ));
                }
            }
        }
        if let Some(parent) = &self.backing_qcow {
            parent.validate_inner(budget)?;
        }
        Ok(())
    }
}
