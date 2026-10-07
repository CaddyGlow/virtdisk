//! Native disk snapshot publication through the bounded QCOW2 redo journal.
use super::{
    COPIED, LockedSource, MASK, Qcow2Writer,
    allocator::Builder,
    journal::{self, Patch, Record},
};
use crate::{Qcow2, Qcow2Snapshot, ReadAt};
use std::{collections::BTreeMap, io, sync::Arc};
const CLUSTER: u64 = 65536;
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
impl Qcow2Writer {
    /// Delete one native disk snapshot without changing surviving disk states.
    /// Linux standalone bounded disk-only profiles are required. The complete
    /// metadata update is journaled; interrupted publication requires reopening.
    pub fn delete_snapshot(&mut self, id: &[u8]) -> io::Result<Qcow2Snapshot> {
        self.lifecycle_inner(id, Lifecycle::Delete, None)
    }
    /// Replace the active disk state with a retained native disk snapshot.
    /// The selected snapshot and every sibling remain available. Saved capacity
    /// replaces current capacity; later writes COW shared payload. Linux bounded
    /// standalone disk-only profiles and recoverable metadata updates are required.
    pub fn revert_snapshot(&mut self, id: &[u8]) -> io::Result<Qcow2Snapshot> {
        self.lifecycle_inner(id, Lifecycle::Revert, None)
    }
    fn lifecycle_inner(
        &mut self,
        id: &[u8],
        action: Lifecycle,
        cut: Option<usize>,
    ) -> io::Result<Qcow2Snapshot> {
        let guard = self.operation()?;
        let plan = lifecycle_plan(self, id, action)?;
        // An empty selected/active state can already have the desired metadata.
        // The journal intentionally accepts only nonempty replacement records.
        if plan.record.patches.is_empty() {
            return Ok(plan.snapshot);
        }
        if let Err(error) =
            journal::commit_authorized(&self.context.path, self.raw.clone(), plan.record, cut, &[])
        {
            if journal::sidecar(&self.context.path).exists() {
                self.recovery_required
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            return Err(error);
        }
        let mut mappings = self
            .mappings
            .lock()
            .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
        *mappings = plan.mappings;
        drop(mappings);
        drop(guard);
        self.size = plan.size;
        self.native_snapshot_count = plan.count;
        self.snapshot_creation_profile = plan.creation_profile;
        Ok(plan.snapshot)
    }

    /// Create a native internal disk-only snapshot with an explicit unique ID.
    ///
    /// Linux, standalone v3/64 KiB/16-bit ownership and one-cluster L1/directory
    /// profiles are required. IDs and names are limited to 256 bytes, with at
    /// most 64 snapshots. The bounded redo journal makes metadata recoverable;
    /// failed publication requires reopening. Creation does not capture VM state.
    /// Later active writes copy shared allocations and preserve saved disk bytes.
    pub fn create_snapshot(&mut self, id: &[u8], name: &[u8]) -> io::Result<Qcow2Snapshot> {
        self.create_snapshot_inner(id, name, None)
    }
    fn create_snapshot_inner(
        &mut self,
        id: &[u8],
        name: &[u8],
        cut: Option<usize>,
    ) -> io::Result<Qcow2Snapshot> {
        if !cfg!(target_os = "linux") || self.context.parent.is_some() {
            return Err(unsupported(
                "QCOW2 snapshot creation requires standalone Linux profile",
            ));
        }
        if self.raw.len() > 33 * 1024 * 1024 * 1024 {
            return Err(unsupported(
                "QCOW2 snapshot physical image exceeds 33 GiB journal limit",
            ));
        }
        if id.is_empty() || id.len() > 256 || name.len() > 256 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let _operation = self.operation()?;
        self.raw.require_single_link_for_journal()?;
        let source = Arc::new(LockedSource {
            raw: self.raw.clone(),
            size: self.raw.len(),
        });
        let disk = Qcow2::open_locked_chain(
            source.clone(),
            &self.context.path,
            &[],
            self.raw.file_identity()?,
        )?;
        disk.validate_active_mapping()?;
        let (snapshots, directory_length) = disk.snapshot_directory()?;
        if snapshots.len() >= 64 || snapshots.iter().any(|s| s.vm_state_size != 0) {
            return Err(unsupported(
                "QCOW2 snapshot directory profile exceeds creation bounds",
            ));
        }
        if snapshots.iter().any(|s| s.id == id) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "QCOW2 snapshot ID already exists",
            ));
        }
        let mut header = [0; 104];
        self.raw.read_exact_at(0, &mut header)?;
        let u64_at = |at| u64::from_be_bytes(header[at..at + 8].try_into().unwrap());
        let l1_entries = u32::from_be_bytes(header[36..40].try_into().unwrap());
        if u64::from(l1_entries) * 8 > CLUSTER || directory_length > CLUSTER {
            return Err(unsupported(
                "QCOW2 snapshot L1 or directory exceeds one cluster",
            ));
        }
        let entry_length = (56 + id.len() + name.len()).div_ceil(8) as u64 * 8;
        if directory_length + entry_length > CLUSTER {
            return Err(unsupported("QCOW2 snapshot directory is full"));
        }
        let digest = journal::digest_reader(&*source)?;
        let mut builder = Builder {
            raw: self.raw.clone(),
            original: self.raw.len(),
            end: self.raw.len().div_ceil(CLUSTER) * CLUSTER,
            patches: BTreeMap::new(),
            ref_table: u64_at(48),
            ref_table_clusters: u32::from_be_bytes(header[56..60].try_into().unwrap()) as u64,
        };
        // Preserve any omitted final cluster tail in the redo extent.
        if !builder.original.is_multiple_of(CLUSTER) {
            builder.cluster(builder.original / CLUSTER * CLUSTER)?;
        }
        let saved_l1 = if l1_entries == 0 {
            0
        } else {
            let saved = builder.append()?;
            builder.reference(saved, 1)?;
            let mut l1 = vec![0; CLUSTER as usize];
            self.raw
                .read_exact_at(u64_at(40), &mut l1[..l1_entries as usize * 8])?;
            *builder.cluster(saved)? = l1.clone();
            for (index, entry) in l1[..l1_entries as usize * 8]
                .as_chunks::<8>()
                .0
                .iter()
                .enumerate()
            {
                let raw = u64::from_be_bytes(*entry);
                let l2 = raw & MASK;
                if l2 == 0 {
                    continue;
                }
                builder.reference(l2, 1)?;
                builder.set64(u64_at(40) + index as u64 * 8, raw & !COPIED)?;
                let mut bytes = vec![0; CLUSTER as usize];
                self.raw.read_exact_at(l2, &mut bytes)?;
                for (slot, entry) in bytes.as_chunks::<8>().0.iter().enumerate() {
                    let descriptor = u64::from_be_bytes(*entry);
                    if descriptor & !(MASK | COPIED | 1) != 0 {
                        return Err(unsupported(
                            "QCOW2 compressed active snapshot creation is unsupported",
                        ));
                    }
                    let host = descriptor & MASK;
                    if host != 0 {
                        builder.reference(host, 1)?;
                    }
                    if descriptor & COPIED != 0 {
                        builder.set64(l2 + slot as u64 * 8, descriptor & !COPIED)?;
                    }
                }
            }
            saved
        };
        let directory = builder.append()?;
        builder.reference(directory, 1)?;
        let mut bytes = vec![0; CLUSTER as usize];
        if directory_length != 0 {
            let old = u64_at(64);
            let count = directory_length.min(self.raw.len().saturating_sub(old)) as usize;
            self.raw.read_exact_at(old, &mut bytes[..count])?;
            builder.reference(old, -1)?;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?;
        let date = u32::try_from(now.as_secs())
            .map_err(|_| unsupported("QCOW2 snapshot timestamp exceeds native field"))?;
        let at = directory_length as usize;
        bytes[at..at + 8].copy_from_slice(&saved_l1.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&l1_entries.to_be_bytes());
        bytes[at + 12..at + 14].copy_from_slice(&(id.len() as u16).to_be_bytes());
        bytes[at + 14..at + 16].copy_from_slice(&(name.len() as u16).to_be_bytes());
        bytes[at + 16..at + 20].copy_from_slice(&date.to_be_bytes());
        bytes[at + 20..at + 24].copy_from_slice(&now.subsec_nanos().to_be_bytes());
        bytes[at + 36..at + 40].copy_from_slice(&16u32.to_be_bytes());
        bytes[at + 48..at + 56].copy_from_slice(&self.size.to_be_bytes());
        bytes[at + 56..at + 56 + id.len()].copy_from_slice(id);
        bytes[at + 56 + id.len()..at + 56 + id.len() + name.len()].copy_from_slice(name);
        *builder.cluster(directory)? = bytes;
        builder.cluster(0)?[60..64].copy_from_slice(&((snapshots.len() + 1) as u32).to_be_bytes());
        builder.set64(64, directory)?;
        let mut patches = Vec::new();
        for (offset, new) in builder.patches {
            let mut old = vec![0; (builder.original.saturating_sub(offset).min(CLUSTER)) as usize];
            if !old.is_empty() {
                self.raw.read_exact_at(offset, &mut old)?;
            }
            if old != new {
                patches.push(Patch { offset, old, new });
            }
        }
        let record = Record {
            original_length: builder.original,
            final_length: builder.end,
            original_digest: digest,
            patches,
        };
        if let Err(error) =
            journal::commit_authorized(&self.context.path, self.raw.clone(), record, cut, &[])
        {
            if journal::sidecar(&self.context.path).exists() {
                self.recovery_required
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            return Err(error);
        }
        let mut mappings = self
            .mappings
            .lock()
            .map_err(|_| io::Error::other("QCOW2 mapping mutex poisoned"))?;
        for mapping in mappings.iter_mut() {
            *mapping &= !COPIED;
        }
        drop(mappings);
        drop(_operation);
        self.native_snapshot_count += 1;
        self.snapshot_creation_profile &= directory_length + entry_length <= CLUSTER - 64;
        Ok(Qcow2Snapshot {
            id: id.to_vec(),
            name: name.to_vec(),
            virtual_size: self.size,
            vm_state_size: 0,
            date_seconds: date,
            date_nanoseconds: now.subsec_nanos(),
            vm_clock_nanoseconds: 0,
            l1_table_offset: saved_l1,
            l1_entries,
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod recovery_tests {
    use super::*;
    use crate::{RawDisk, ReadAt};
    #[test]
    fn native_snapshot_creation_recovers_each_persistence_and_metadata_patch_cut() {
        let _test_guard = crate::test_sync::writer_test();
        for cut in (0..=7).chain(100..106) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk.qcow2");
            let mut writer = Qcow2Writer::create_sparse(&path, 131072).unwrap();
            writer.write_all_at(500, b"before").unwrap();
            writer.flush().unwrap();
            assert!(
                writer
                    .create_snapshot_inner(b"saved", b"native", Some(cut))
                    .is_err(),
                "cut {cut}"
            );
            assert!(writer.read_exact_at(0, &mut [0]).is_err());
            assert!(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).is_err());
            drop(writer);
            let writer = Qcow2Writer::open(&path).unwrap();
            writer.write_all_at(500, b"after!").unwrap();
            writer.flush().unwrap();
            drop(writer);
            let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
            disk.validate_active_mapping().unwrap();
            assert_eq!(disk.list_snapshots().unwrap().len(), 1);
            let saved = disk.open_snapshot(b"saved").unwrap();
            let mut bytes = [0; 6];
            saved.read_exact_at(500, &mut bytes).unwrap();
            assert_eq!(&bytes, b"before", "cut {cut}");
            disk.read_exact_at(500, &mut bytes).unwrap();
            assert_eq!(&bytes, b"after!");
            drop(saved);
            drop(disk);
            assert!(!journal::sidecar(&path).exists());
            drop(Qcow2Writer::open(&path).unwrap());
        }
    }
}

#[derive(Clone, Copy)]
enum Lifecycle {
    Delete,
    Revert,
}
struct LifecyclePlan {
    snapshot: Qcow2Snapshot,
    record: Record,
    mappings: Vec<u64>,
    size: u64,
    count: u32,
    creation_profile: bool,
}
struct Work(u64);
impl Work {
    fn step(&mut self) -> io::Result<()> {
        self.0 += 1;
        if self.0 > 16 * 1024 * 1024 {
            return Err(unsupported("QCOW2 snapshot lifecycle work limit exceeded"));
        }
        Ok(())
    }
}
fn l1_bytes(writer: &Qcow2Writer, offset: u64, entries: u32) -> io::Result<Vec<u8>> {
    if u64::from(entries) * 8 > CLUSTER {
        return Err(unsupported(
            "QCOW2 snapshot lifecycle requires one-cluster L1 tables",
        ));
    }
    let mut bytes = vec![0; entries as usize * 8];
    if !bytes.is_empty() {
        writer.raw.read_exact_at(offset, &mut bytes)?;
    }
    Ok(bytes)
}
fn delta(deltas: &mut BTreeMap<u64, i32>, offset: u64, change: i32) -> io::Result<()> {
    let value = deltas.entry(offset).or_default();
    *value = value
        .checked_add(change)
        .ok_or(io::ErrorKind::InvalidData)?;
    Ok(())
}
fn map_deltas(
    writer: &Qcow2Writer,
    l1: &[u8],
    change: i32,
    deltas: &mut BTreeMap<u64, i32>,
    work: &mut Work,
) -> io::Result<()> {
    let mut table = vec![0; CLUSTER as usize];
    for entry in l1.as_chunks::<8>().0 {
        work.step()?;
        let offset = u64::from_be_bytes(*entry) & MASK;
        if offset == 0 {
            continue;
        }
        delta(deltas, offset, change)?;
        writer.raw.read_exact_at(offset, &mut table)?;
        for entry in table.as_chunks::<8>().0 {
            work.step()?;
            let raw = u64::from_be_bytes(*entry) & !COPIED;
            if raw & (1 << 62) != 0 {
                // Standard 64 KiB compressed descriptors encode a sector count
                // in bits 54..61 and an unaligned physical start below bit 54.
                let host = raw & ((1u64 << 54) - 1);
                let sectors = ((raw & !(1u64 << 62)) >> 54) + 1;
                let end = host + sectors * 512 - host % 512;
                for cluster in host / CLUSTER..(end.div_ceil(CLUSTER)) {
                    work.step()?;
                    delta(deltas, cluster * CLUSTER, change)?;
                }
            } else {
                if raw & !(MASK | 1) != 0 {
                    return Err(unsupported("QCOW2 reserved selected descriptor bits"));
                }
                let host = raw & MASK;
                if host != 0 {
                    delta(deltas, host, change)?;
                }
            }
        }
    }
    Ok(())
}
fn lifecycle_plan(writer: &Qcow2Writer, id: &[u8], action: Lifecycle) -> io::Result<LifecyclePlan> {
    if !cfg!(target_os = "linux")
        || writer.context.parent.is_some()
        || writer.raw.len() > 33 * 1024 * 1024 * 1024
    {
        return Err(unsupported(
            "QCOW2 snapshot lifecycle requires bounded standalone Linux profile",
        ));
    }
    writer.raw.require_single_link_for_journal()?;
    let source = Arc::new(LockedSource {
        raw: writer.raw.clone(),
        size: writer.raw.len(),
    });
    let disk = Qcow2::open_locked_chain(
        source.clone(),
        &writer.context.path,
        &[],
        writer.raw.file_identity()?,
    )?;
    disk.validate_active_mapping()?;
    let (snapshots, directory_length) = disk.snapshot_directory()?;
    let selected = snapshots
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "QCOW2 snapshot ID not found"))?;
    if snapshots.len() > 64 || directory_length > CLUSTER {
        return Err(unsupported(
            "QCOW2 snapshot lifecycle directory exceeds bounded profile",
        ));
    }
    let snapshot = snapshots[selected].clone();
    if snapshots.iter().any(|s| s.vm_state_size != 0) {
        return Err(unsupported(
            "QCOW2 snapshot lifecycle VM-state ownership unsupported",
        ));
    }
    let mut header = [0; 104];
    writer.raw.read_exact_at(0, &mut header)?;
    let value = |at| u64::from_be_bytes(header[at..at + 8].try_into().unwrap());
    let old_entries = u32::from_be_bytes(header[36..40].try_into().unwrap());
    let old_l1 = l1_bytes(writer, value(40), old_entries)?;
    let selected_l1 = l1_bytes(writer, snapshot.l1_table_offset, snapshot.l1_entries)?;
    let size = if matches!(action, Lifecycle::Revert) {
        Qcow2Writer::check_size(snapshot.virtual_size)?;
        snapshot.virtual_size
    } else {
        writer.size
    };
    let mut deltas = BTreeMap::new();
    let mut work = Work(0);
    let mut directory_bytes = vec![0; directory_length as usize];
    let directory_read = directory_length.min(writer.raw.len().saturating_sub(value(64))) as usize;
    writer
        .raw
        .read_exact_at(value(64), &mut directory_bytes[..directory_read])?;
    let mut replacement_directory = Vec::new();
    let (count, directory_final) = if matches!(action, Lifecycle::Delete) {
        map_deltas(writer, &selected_l1, -1, &mut deltas, &mut work)?;
        if !selected_l1.is_empty() {
            delta(&mut deltas, snapshot.l1_table_offset, -1)?;
        }
        let mut at = 0usize;
        for index in 0..snapshots.len() {
            let extra =
                u32::from_be_bytes(directory_bytes[at + 36..at + 40].try_into().unwrap()) as usize;
            let id_size =
                u16::from_be_bytes(directory_bytes[at + 12..at + 14].try_into().unwrap()) as usize;
            let name_size =
                u16::from_be_bytes(directory_bytes[at + 14..at + 16].try_into().unwrap()) as usize;
            let length = (40 + extra + id_size + name_size).div_ceil(8) * 8;
            if index != selected {
                replacement_directory.extend_from_slice(&directory_bytes[at..at + length]);
            }
            at += length;
        }
        delta(&mut deltas, value(64), -1)?;
        (
            (snapshots.len() - 1) as u32,
            replacement_directory.len() as u64,
        )
    } else {
        map_deltas(writer, &old_l1, -1, &mut deltas, &mut work)?;

        if !old_l1.is_empty() {
            delta(&mut deltas, value(40), -1)?;
        }
        (snapshots.len() as u32, directory_length)
    };
    let mut b = Builder {
        raw: writer.raw.clone(),
        original: writer.raw.len(),
        end: writer.raw.len().div_ceil(CLUSTER) * CLUSTER,
        patches: BTreeMap::new(),
        ref_table: value(48),
        ref_table_clusters: u32::from_be_bytes(header[56..60].try_into().unwrap()) as u64,
    };
    if !b.original.is_multiple_of(CLUSTER) {
        b.cluster(b.original / CLUSTER * CLUSTER)?;
    }
    let (active_offset, mut active_l1) = if matches!(action, Lifecycle::Revert) {
        if selected_l1.is_empty() {
            (0, Vec::new())
        } else {
            let offset = b.append()?;
            delta(&mut deltas, offset, 1)?;
            (offset, selected_l1)
        }
    } else {
        (value(40), old_l1)
    };
    if matches!(action, Lifecycle::Revert) {
        // Saved compressed mappings remain immutable. Materialize only the
        // active references, with all payload and table publication journaled.
        let saved = Arc::new(disk).open_snapshot(id)?;
        for (index, entry) in active_l1.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            work.step()?;
            let old = u64::from_be_bytes(*entry) & MASK;
            if old == 0 {
                continue;
            }
            let mut table = vec![0; CLUSTER as usize];
            writer.raw.read_exact_at(old, &mut table)?;
            let mut compressed = false;
            for word in table.as_chunks::<8>().0 {
                work.step()?;
                compressed |= u64::from_be_bytes(*word) & (1 << 62) != 0;
            }
            if compressed {
                let table_offset = b.append()?;
                delta(&mut deltas, table_offset, 1)?;
                for (slot, word) in table.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                    work.step()?;
                    let raw = u64::from_be_bytes(*word) & !COPIED;
                    if raw & (1 << 62) != 0 {
                        let guest = (index as u64 * 8192 + slot as u64) * CLUSTER;
                        if guest >= size {
                            return Err(unsupported(
                                "compressed saved mappings outside virtual capacity",
                            ));
                        }
                        let payload = b.append()?;
                        let count = (size - guest).min(CLUSTER) as usize;
                        saved.read_exact_at(guest, &mut b.cluster(payload)?[..count])?;
                        delta(&mut deltas, payload, 1)?;
                        word.copy_from_slice(&payload.to_be_bytes());
                    } else if raw & MASK != 0 {
                        delta(&mut deltas, raw & MASK, 1)?;
                    }
                }
                *b.cluster(table_offset)? = table;
                entry.copy_from_slice(&table_offset.to_be_bytes());
            } else {
                delta(&mut deltas, old, 1)?;
                for word in table.as_chunks::<8>().0 {
                    work.step()?;
                    let host = u64::from_be_bytes(*word) & MASK;
                    if host != 0 {
                        delta(&mut deltas, host, 1)?;
                    }
                }
            }
        }
    }
    let directory = if matches!(action, Lifecycle::Delete) {
        if count == 0 {
            0
        } else {
            let offset = b.append()?;
            delta(&mut deltas, offset, 1)?;
            b.cluster(offset)?[..replacement_directory.len()]
                .copy_from_slice(&replacement_directory);
            offset
        }
    } else {
        value(64)
    };
    for (offset, change) in deltas {
        if change != 0 {
            b.reference(offset, change)?;
        }
    }
    let mut mappings = vec![0; size.div_ceil(CLUSTER) as usize];
    for (index, entry) in active_l1.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        work.step()?;
        let mut raw = u64::from_be_bytes(*entry) & !COPIED;
        let offset = raw & MASK;
        if offset == 0 {
            entry.copy_from_slice(&raw.to_be_bytes());
            continue;
        }
        if b.reference(offset, 0)? == 1 {
            raw |= COPIED;
        }
        entry.copy_from_slice(&raw.to_be_bytes());
        let mut table = b.cluster(offset)?.clone();
        for (slot, entry) in table.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            work.step()?;
            let mut raw = u64::from_be_bytes(*entry) & !COPIED;
            if raw & !(MASK | 1) != 0 {
                return Err(unsupported(
                    "QCOW2 compressed active snapshot lifecycle unsupported",
                ));
            }
            let host = raw & MASK;
            if host != 0 && b.reference(host, 0)? == 1 {
                raw |= COPIED;
            }
            entry.copy_from_slice(&raw.to_be_bytes());
            if let Some(mapping) = mappings.get_mut(index * 8192 + slot) {
                *mapping = raw;
            }
        }
        *b.cluster(offset)? = table;
    }
    if !active_l1.is_empty() {
        b.cluster(active_offset)?[..active_l1.len()].copy_from_slice(&active_l1);
    }
    let active_entries = (active_l1.len() / 8) as u32;
    let bytes = b.cluster(0)?;
    bytes[24..32].copy_from_slice(&size.to_be_bytes());
    bytes[36..40].copy_from_slice(&active_entries.to_be_bytes());
    bytes[40..48].copy_from_slice(&active_offset.to_be_bytes());
    bytes[60..64].copy_from_slice(&count.to_be_bytes());
    bytes[64..72].copy_from_slice(&directory.to_be_bytes());
    let mut patches = Vec::new();
    for (offset, new) in b.patches {
        let mut old = vec![0; b.original.saturating_sub(offset).min(CLUSTER) as usize];
        if !old.is_empty() {
            writer.raw.read_exact_at(offset, &mut old)?;
        }
        if old != new {
            patches.push(Patch { offset, old, new });
        }
    }
    if b.end > 33 * 1024 * 1024 * 1024 {
        return Err(unsupported(
            "QCOW2 lifecycle proposed image exceeds physical limit",
        ));
    }
    let record = Record {
        original_length: b.original,
        final_length: b.end,
        original_digest: journal::digest_reader(&*source)?,
        patches,
    };
    Ok(LifecyclePlan {
        snapshot,
        record,
        mappings,
        size,
        count,
        creation_profile: count < 64 && directory_final <= CLUSTER - 64,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod lifecycle_recovery_tests {
    use super::*;
    use crate::{RawDisk, ReadAt};
    fn states(single: bool) -> &'static [(&'static [u8], u8)] {
        if single {
            &[(b"a", 17)]
        } else {
            &[(b"a", 17), (b"b", 29), (b"c", 41)]
        }
    }
    fn prepare(path: &std::path::Path, shared: bool, single: bool, smaller: bool) -> Qcow2Writer {
        let mut writer = Qcow2Writer::create_sparse(path, 131072).unwrap();
        for &(id, byte) in states(single) {
            writer.write_all_at(500, &[byte; 12]).unwrap();
            writer.create_snapshot(id, b"state").unwrap();
        }
        if !shared {
            writer.write_all_at(500, &[63; 12]).unwrap();
        }
        writer.flush().unwrap();
        if smaller {
            let mut offset = [0; 8];
            writer.raw.read_exact_at(64, &mut offset).unwrap();
            // The one-byte-ID/five-byte-name native records have a 64-byte stride.
            writer
                .raw
                .write_all_at(
                    u64::from_be_bytes(offset) + 64 + 48,
                    &65536u64.to_be_bytes(),
                )
                .unwrap();
            writer.raw.flush().unwrap();
        }
        writer
    }
    fn model(byte: u8, size: usize) -> Vec<u8> {
        let mut out = vec![0; size];
        out[500..512].fill(byte);
        out
    }
    fn compress_saved_fixture(path: &std::path::Path, crossing: bool) -> Qcow2Writer {
        use std::io::Write;
        let writer = prepare(path, false, true, false);
        let source = Arc::new(LockedSource {
            raw: writer.raw.clone(),
            size: writer.raw.len(),
        });
        let disk = Qcow2::open(source).unwrap();
        let snapshot = disk.list_snapshots().unwrap().remove(0);
        let mut bytes = [0; 8];
        writer
            .raw
            .read_exact_at(snapshot.l1_table_offset, &mut bytes)
            .unwrap();
        let table = u64::from_be_bytes(bytes) & MASK;
        writer.raw.read_exact_at(table, &mut bytes).unwrap();
        let payload = u64::from_be_bytes(bytes) & MASK;
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&model(17, CLUSTER as usize)).unwrap();
        let packed = encoder.finish().unwrap();
        let host = if crossing {
            let mut header = [0; 104];
            writer.raw.read_exact_at(0, &mut header).unwrap();
            let mut b = Builder {
                raw: writer.raw.clone(),
                original: writer.raw.len(),
                end: writer.raw.len().div_ceil(CLUSTER) * CLUSTER,
                patches: BTreeMap::new(),
                ref_table: u64::from_be_bytes(header[48..56].try_into().unwrap()),
                ref_table_clusters: u32::from_be_bytes(header[56..60].try_into().unwrap()) as u64,
            };
            let first = b.append().unwrap();
            let second = b.append().unwrap();
            b.reference(first, 1).unwrap();
            b.reference(second, 1).unwrap();
            b.reference(payload, -1).unwrap();
            writer.raw.resize(b.end).unwrap();
            for (offset, bytes) in b.patches {
                writer.raw.write_all_at(offset, &bytes).unwrap();
            }
            first + CLUSTER - 17
        } else {
            payload + 17
        };
        let sectors = (packed.len() as u64 + host % 512).div_ceil(512);
        writer.raw.write_all_at(host, &packed).unwrap();
        let descriptor = (1u64 << 62) | ((sectors - 1) << 54) | host;
        writer
            .raw
            .write_all_at(table, &descriptor.to_be_bytes())
            .unwrap();
        writer.raw.flush().unwrap();
        let source = Arc::new(LockedSource {
            raw: writer.raw.clone(),
            size: writer.raw.len(),
        });
        Qcow2::open(source)
            .unwrap()
            .validate_active_mapping_and_compressed_payloads()
            .unwrap();
        writer
    }
    #[test]
    fn compressed_lifecycle_recovers_each_payload_metadata_and_persistence_cut() {
        let _guard = crate::test_sync::writer_test();
        let mut cuts = 0;
        for (action, crossing) in [
            (Lifecycle::Delete, false),
            (Lifecycle::Revert, false),
            (Lifecycle::Delete, true),
            (Lifecycle::Revert, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let template = dir.path().join("compressed.qcow2");
            let writer = compress_saved_fixture(&template, crossing);
            let patches = lifecycle_plan(&writer, b"a", action)
                .unwrap()
                .record
                .patches
                .len();
            drop(writer);
            for cut in (0..=7).chain(100..100 + patches) {
                cuts += 1;
                let path = dir.path().join(format!("cut-{cut}.qcow2"));
                std::fs::copy(&template, &path).unwrap();
                let mut writer = Qcow2Writer::open(&path).unwrap();
                assert!(writer.lifecycle_inner(b"a", action, Some(cut)).is_err());
                drop(writer);
                let writer = Qcow2Writer::open(&path).unwrap();
                let mut actual = vec![0; 131072];
                writer.read_exact_at(0, &mut actual).unwrap();
                assert_eq!(
                    actual,
                    model(
                        if matches!(action, Lifecycle::Revert) {
                            17
                        } else {
                            63
                        },
                        131072
                    )
                );
                writer.write_all_at(505, &[71]).unwrap();
                writer.flush().unwrap();
                drop(writer);
                let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
                disk.validate_active_mapping_and_compressed_payloads()
                    .unwrap();
                if matches!(action, Lifecycle::Revert) {
                    disk.open_snapshot(b"a")
                        .unwrap()
                        .read_exact_at(0, &mut actual)
                        .unwrap();
                    assert_eq!(actual, model(17, 131072));
                } else {
                    assert!(disk.list_snapshots().unwrap().is_empty());
                }
                assert!(!journal::sidecar(&path).exists());
            }
        }
        eprintln!("verified {cuts} compressed lifecycle cuts");
    }
    #[test]
    fn delete_and_revert_recover_every_actual_metadata_patch_and_persistence_cut() {
        let _guard = crate::test_sync::writer_test();
        let mut cuts = 0;
        for (action, id, shared, single, smaller) in [
            (Lifecycle::Delete, b"b".as_slice(), false, false, false),
            (Lifecycle::Delete, b"c", true, false, false),
            (Lifecycle::Delete, b"a", true, true, false),
            (Lifecycle::Revert, b"b", false, false, false),
            (Lifecycle::Revert, b"b", false, false, true),
        ] {
            let sample = tempfile::tempdir().unwrap();
            let template = sample.path().join("count.qcow2");
            let writer = prepare(&template, shared, single, smaller);
            let patches = lifecycle_plan(&writer, id, action)
                .unwrap()
                .record
                .patches
                .len();
            drop(writer);
            for cut in (0..=7).chain(100..100 + patches) {
                cuts += 1;
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("fault.qcow2");
                std::fs::copy(&template, &path).unwrap();
                let mut writer = Qcow2Writer::open(&path).unwrap();
                assert!(
                    writer.lifecycle_inner(id, action, Some(cut)).is_err(),
                    "cut {cut}"
                );
                assert!(writer.read_exact_at(0, &mut [0]).is_err());
                assert!(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).is_err());
                drop(writer);
                let writer = Qcow2Writer::open(&path).unwrap();
                let byte = if matches!(action, Lifecycle::Revert) {
                    29
                } else if shared {
                    if single { 17 } else { 41 }
                } else {
                    63
                };
                let size = if smaller { 65536 } else { 131072 };
                assert_eq!(writer.len(), size as u64);
                let mut expected = model(byte, size);
                let mut actual = vec![0; size];
                writer.read_exact_at(0, &mut actual).unwrap();
                assert_eq!(actual, expected, "cut {cut}");
                writer.write_all_at(505, &[71]).unwrap();
                expected[505] = 71;
                writer.flush().unwrap();
                drop(writer);
                let disk = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
                disk.validate_active_mapping().unwrap();
                disk.read_exact_at(0, &mut actual).unwrap();
                assert_eq!(actual, expected);
                assert_eq!(
                    disk.list_snapshots().unwrap().len(),
                    states(single).len() - usize::from(matches!(action, Lifecycle::Delete))
                );
                for &(state_id, byte) in states(single) {
                    if matches!(action, Lifecycle::Delete) && state_id == id {
                        assert!(disk.open_snapshot(state_id).is_err());
                        continue;
                    }
                    let view = disk.open_snapshot(state_id).unwrap();
                    let saved_size = if smaller && state_id == b"b" {
                        65536
                    } else {
                        131072
                    };
                    assert_eq!(view.len(), saved_size as u64);
                    actual.resize(saved_size, 0);
                    view.read_exact_at(0, &mut actual).unwrap();
                    assert_eq!(actual, model(byte, saved_size), "cut {cut}");
                }
                drop(disk);
                assert!(!journal::sidecar(&path).exists());
                drop(Qcow2Writer::open(&path).unwrap());
            }
        }
        eprintln!("verified {cuts} lifecycle persistence/metadata cuts");
    }
}
