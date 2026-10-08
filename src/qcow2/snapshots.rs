//! Bounded internal snapshot directory inspection.
use super::{Qcow2, be32, be64, invalid, unsupported, zeroed_bytes};
use crate::io;
use crate::{ReadAt, check_range};
use alloc::{sync::Arc, vec::Vec};
#[cfg(feature = "std")]
use std::{collections::HashMap, sync::Mutex};
/// Immutable saved disk state, opened after complete container ownership validation.
/// Sources and authorized backing files must remain immutable for its lifetime.
pub struct Qcow2SnapshotView {
    image: Qcow2,
    snapshot: Qcow2Snapshot,
    _owner: Arc<Qcow2>,
}
impl Qcow2SnapshotView {
    /// Validated directory metadata identifying the selected saved disk state.
    pub fn snapshot(&self) -> &Qcow2Snapshot {
        &self.snapshot
    }
}
impl ReadAt for Qcow2SnapshotView {
    fn source_identity(&self) -> Option<crate::SourceIdentity> {
        self.image.source_identity()
    }
    fn ancestor_identities(&self) -> Vec<crate::SourceIdentity> {
        self.image.ancestor_identities()
    }
    fn host_context(&self) -> Option<&dyn core::any::Any> {
        self.image.host_context()
    }
    fn len(&self) -> u64 {
        self.image.len()
    }
    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.image.read_exact_at(offset, destination)
    }
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(crate::DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        self.image.visit_extents(visitor)
    }
    fn context(&self) -> crate::ReadContext {
        self.image.context()
    }
    fn budget(&self) -> Option<crate::ReadBudget> {
        self.image.budget()
    }
}

/// Metadata for one internal QCOW2 snapshot; mapping ownership is not audited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qcow2Snapshot {
    /// Unique identifier bytes; no UTF-8 interpretation is imposed.
    pub id: Vec<u8>,
    /// Display-name bytes; not necessarily UTF-8.
    pub name: Vec<u8>,
    /// Saved virtual disk capacity in bytes.
    pub virtual_size: u64,
    /// Saved VM memory/state size; listing does not interpret that state.
    pub vm_state_size: u64,
    /// Creation time in seconds since the Unix epoch.
    pub date_seconds: u32,
    /// Subsecond creation time in nanoseconds.
    pub date_nanoseconds: u32,
    /// Saved guest runtime in nanoseconds.
    pub vm_clock_nanoseconds: u64,
    /// Snapshot L1 table location in the container.
    pub l1_table_offset: u64,
    /// Number of snapshot L1 table entries.
    pub l1_entries: u32,
}

impl Qcow2 {
    /// Open a saved disk state by exact identifier bytes, preserving the active state.
    ///
    /// Validates active and snapshot-owned metadata/refcounts before exposing bytes.
    /// VM-state snapshots remain unsupported. The view retains the original image
    /// and authorized backing readers; it provides no mutation or restore operation.
    /// Caller budgets apply cumulatively to listing, ownership validation and reads.
    pub fn open_snapshot(self: &Arc<Self>, id: &[u8]) -> io::Result<Qcow2SnapshotView> {
        let snapshot = self
            .list_snapshots()?
            .into_iter()
            .find(|snapshot| snapshot.id == id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "QCOW2 snapshot identifier not found",
                )
            })?;
        self.validate_active_mapping()?;
        let image = Self {
            source: self.source.clone(),
            #[cfg(feature = "std")]
            entry_cache: Mutex::new(HashMap::new()),
            version: self.version,
            size: snapshot.virtual_size,
            cluster_size: self.cluster_size,
            l1_offset: snapshot.l1_table_offset,
            l1_size: u64::from(snapshot.l1_entries),
            compression: self.compression,
            backing_name: self.backing_name.clone(),
            backing_format: self.backing_format.clone(),
            backing: self.backing.clone(),
            backing_qcow: self.backing_qcow.clone(),
            refcount_offset: self.refcount_offset,
            refcount_clusters: self.refcount_clusters,
            refcount_order: self.refcount_order,
            snapshots: 0,
            snapshots_offset: 0,
            inactive_mapping: true,
            extra_metadata: self.extra_metadata,
        };
        Ok(Qcow2SnapshotView {
            image,
            snapshot,
            _owner: self.clone(),
        })
    }
    /// Inspect at most 1024 internal snapshot directory entries and 1 MiB of metadata.
    ///
    /// Validates directory bounds, alignment, identifiers, timestamps and L1
    /// coverage. Unknown bounded extra fields are ignored. Does not validate
    /// snapshot allocation ownership or expose saved disk/VM state. Use
    /// `validate_active_mapping` to audit all disk-state owners, or
    /// `open_snapshot` for an audited read-only saved disk view. VM-state
    /// ownership is unsupported. Sources must remain immutable; caller budgets apply.
    pub fn list_snapshots(&self) -> io::Result<Vec<Qcow2Snapshot>> {
        self.snapshot_directory().map(|(entries, _)| entries)
    }

    pub(crate) fn snapshot_directory(&self) -> io::Result<(Vec<Qcow2Snapshot>, u64)> {
        if self.snapshots > 1024 {
            return Err(unsupported("QCOW2 snapshot count exceeds inspection limit"));
        }
        if self.snapshots == 0 {
            return Ok((Vec::new(), 0));
        }
        if self.snapshots_offset == 0 || !self.snapshots_offset.is_multiple_of(self.cluster_size) {
            return Err(invalid("invalid QCOW2 snapshot table alignment"));
        }
        let mut position = self.snapshots_offset;
        let mut total = 0u64;
        let mut result: Vec<Qcow2Snapshot> = Vec::new();
        for _ in 0..self.snapshots {
            let mut header = [0; 40];
            self.source.read_exact_at(position, &mut header)?;
            let id_length = u64::from(u16::from_be_bytes([header[12], header[13]]));
            let name_length = u64::from(u16::from_be_bytes([header[14], header[15]]));
            let extra_length = u64::from(be32(&header[36..40]));
            let content_length = 40 + extra_length + id_length + name_length;
            let length = content_length.div_ceil(8) * 8;
            total = total
                .checked_add(length)
                .ok_or_else(|| invalid("QCOW2 snapshot directory overflow"))?;
            if total > 1048576 {
                return Err(unsupported(
                    "QCOW2 snapshot directory exceeds inspection limit",
                ));
            }
            // QEMU aligns the next entry but may omit final trailing padding.
            check_range(position, content_length, self.source.len())?;
            if self.version == 3 && extra_length < 16 {
                return Err(invalid("QCOW2 v3 snapshot lacks required extra fields"));
            }
            if let Some(budget) = self.source.budget() {
                budget.metadata(length)?;
            }
            let mut bytes = zeroed_bytes((content_length - 40) as usize)?;
            self.source.read_exact_at(position + 40, &mut bytes)?;
            let vm_state_size = if extra_length >= 8 {
                be64(&bytes[..8])
            } else {
                u64::from(be32(&header[32..36]))
            };
            let virtual_size = if extra_length >= 16 {
                be64(&bytes[8..16])
            } else {
                self.size
            };
            let l1_table_offset = be64(&header[..8]);
            let l1_entries = be32(&header[8..12]);
            let coverage = u64::from(l1_entries)
                .checked_mul(self.cluster_size / 8)
                .and_then(|n| n.checked_mul(self.cluster_size))
                .ok_or_else(|| invalid("QCOW2 snapshot L1 coverage overflow"))?;
            if virtual_size > coverage || be32(&header[20..24]) >= 1000000000 {
                return Err(invalid("invalid QCOW2 snapshot size or timestamp"));
            }
            if l1_entries != 0 {
                Self::cluster_range(
                    &*self.source,
                    l1_table_offset,
                    u64::from(l1_entries) * 8,
                    self.cluster_size,
                )?;
            } else if l1_table_offset != 0 {
                return Err(invalid("empty QCOW2 snapshot L1 has a pointer"));
            }
            let id_start = extra_length as usize;
            let id_end = id_start + id_length as usize;
            let id = bytes[id_start..id_end].to_vec();
            if let Some(budget) = self.source.budget() {
                budget.work(result.len() as u64 + 1)?;
                budget.metadata(core::mem::size_of::<Qcow2Snapshot>() as u64)?;
            }
            if id.is_empty() || result.iter().any(|snapshot| snapshot.id == id) {
                return Err(invalid("invalid or duplicate QCOW2 snapshot identifier"));
            }
            result.try_reserve(1).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "snapshot directory allocation failed",
                )
            })?;
            result.push(Qcow2Snapshot {
                id,
                name: bytes[id_end..id_end + name_length as usize].to_vec(),
                virtual_size,
                vm_state_size,
                date_seconds: be32(&header[16..20]),
                date_nanoseconds: be32(&header[20..24]),
                vm_clock_nanoseconds: be64(&header[24..32]),
                l1_table_offset,
                l1_entries,
            });
            position += length;
        }
        Ok((result, total))
    }
}
