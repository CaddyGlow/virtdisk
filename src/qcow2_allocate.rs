//! Bounded append-only QCOW2 allocation transactions.
use super::{
    COPIED, LockedSource, MASK,
    journal::{self, Patch, Record},
};
use crate::RawWriter;
use crate::io;
use std::{collections::BTreeMap, sync::Arc};
const CLUSTER: u64 = 65536;

pub(super) struct Builder {
    pub(super) raw: Arc<RawWriter>,
    pub(super) original: u64,
    pub(super) end: u64,
    pub(super) patches: BTreeMap<u64, Vec<u8>>,
    pub(super) ref_table: u64,
    pub(super) ref_table_clusters: u64,
}
impl Builder {
    pub(super) fn cluster(&mut self, offset: u64) -> io::Result<&mut Vec<u8>> {
        if !offset.is_multiple_of(CLUSTER) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let full = self.patches.len() >= 16;
        match self.patches.entry(offset) {
            std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::btree_map::Entry::Vacant(entry) => {
                if full {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "QCOW2 transaction exceeds cluster budget",
                    ));
                }
                let mut data = vec![0; CLUSTER as usize];
                if offset < self.original {
                    let count = (self.original - offset).min(CLUSTER) as usize;
                    self.raw.read_exact_at(offset, &mut data[..count])?;
                }
                Ok(entry.insert(data))
            }
        }
    }
    pub(super) fn read64(&mut self, offset: u64) -> io::Result<u64> {
        let start = (offset % CLUSTER) as usize;
        Ok(u64::from_be_bytes(
            self.cluster(offset / CLUSTER * CLUSTER)?[start..start + 8]
                .try_into()
                .unwrap(),
        ))
    }
    pub(super) fn set64(&mut self, offset: u64, value: u64) -> io::Result<()> {
        let start = (offset % CLUSTER) as usize;
        self.cluster(offset / CLUSTER * CLUSTER)?[start..start + 8]
            .copy_from_slice(&value.to_be_bytes());
        Ok(())
    }
    pub(super) fn append(&mut self) -> io::Result<u64> {
        let offset = self.end;
        self.end = self
            .end
            .checked_add(CLUSTER)
            .ok_or(io::ErrorKind::InvalidData)?;
        self.cluster(offset)?;
        Ok(offset)
    }
    pub(super) fn reference(&mut self, offset: u64, delta: i32) -> io::Result<u16> {
        let index = offset / CLUSTER;
        let table_index = index / 32768;
        if table_index >= self.ref_table_clusters * 8192 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "QCOW2 refcount-table relocation is not implemented",
            ));
        }
        let table_offset = self.ref_table + table_index * 8;
        let mut block = self.read64(table_offset)?;
        if block == 0 {
            block = self.append()?;
            self.set64(table_offset, block)?;
            self.reference(block, 1)?;
        }
        let start = ((index % 32768) * 2) as usize;
        let bytes = self.cluster(block)?;
        let current = u16::from_be_bytes(bytes[start..start + 2].try_into().unwrap());
        let value = (i32::from(current) + delta)
            .try_into()
            .map_err(|_| io::ErrorKind::InvalidData)?;
        bytes[start..start + 2].copy_from_slice(&u16::to_be_bytes(value));
        Ok(value)
    }
}

pub(super) fn allocate(
    context: &super::WriteContext,
    raw: Arc<RawWriter>,
    guest: u64,
    descriptor: u64,
    mappings: &[u64],
    within: u64,
    data: &[u8],
) -> io::Result<u64> {
    allocate_inner(
        context,
        raw,
        guest,
        descriptor,
        mappings,
        within,
        (data, None),
    )
}

fn allocate_inner(
    context: &super::WriteContext,
    raw: Arc<RawWriter>,
    guest: u64,
    descriptor: u64,
    mappings: &[u64],
    within: u64,
    data_and_cut: (&[u8], Option<usize>),
) -> io::Result<u64> {
    let (data, cut) = data_and_cut;
    mutate_inner(
        context,
        raw,
        guest,
        descriptor,
        mappings,
        (Mutation::Write { within, data }, cut),
    )
}

enum Mutation<'a> {
    Write { within: u64, data: &'a [u8] },
    Discard,
}

pub(super) fn discard(
    context: &super::WriteContext,
    raw: Arc<RawWriter>,
    guest: u64,
    descriptor: u64,
    mappings: &[u64],
) -> io::Result<u64> {
    mutate_inner(
        context,
        raw,
        guest,
        descriptor,
        mappings,
        (Mutation::Discard, None),
    )
}

fn mutate_inner(
    context: &super::WriteContext,
    raw: Arc<RawWriter>,
    guest: u64,
    descriptor: u64,
    mappings: &[u64],
    request: (Mutation<'_>, Option<usize>),
) -> io::Result<u64> {
    let (operation, cut) = request;
    raw.require_single_link_for_journal()?;
    let mut header = [0; 104];
    raw.read_exact_at(0, &mut header)?;
    let u64_at = |offset| u64::from_be_bytes(header[offset..offset + 8].try_into().unwrap());
    let source = LockedSource {
        size: raw.len(),
        raw: raw.clone(),
    };
    let digest = journal::digest_reader(&source)?;
    let mut b = Builder {
        original: raw.len(),
        end: raw.len().div_ceil(CLUSTER) * CLUSTER,
        raw: raw.clone(),
        patches: BTreeMap::new(),
        ref_table: u64_at(48),
        ref_table_clusters: u32::from_be_bytes(header[56..60].try_into().unwrap()) as u64,
    };
    if !b.original.is_multiple_of(CLUSTER) {
        b.cluster(b.original / CLUSTER * CLUSTER)?;
    }
    let replacement = match operation {
        Mutation::Discard => 1,
        Mutation::Write { within, data } => {
            let payload = b.append()?;
            if descriptor & MASK != 0 && descriptor & 1 == 0 {
                let mut prior = vec![0; CLUSTER as usize];
                raw.read_exact_at(descriptor & MASK, &mut prior)?;
                *b.cluster(payload)? = prior;
            }
            if descriptor & MASK == 0
                && descriptor & 1 == 0
                && let Some(parent) = &context.parent
            {
                let offset = guest * CLUSTER;
                let count = parent.len().saturating_sub(offset).min(CLUSTER) as usize;
                if count != 0 {
                    parent.read_exact_at(offset, &mut b.cluster(payload)?[..count])?;
                }
            }
            let start = within as usize;
            b.cluster(payload)?[start..start + data.len()].copy_from_slice(data);
            b.reference(payload, 1)?;
            payload | COPIED
        }
    };
    if descriptor & MASK != 0 {
        let remaining = b.reference(descriptor & MASK, -1)?;
        if remaining == 1 {
            for (index, other) in mappings.iter().enumerate() {
                if index as u64 != guest && other & MASK == descriptor & MASK {
                    let other_l1 = b.read64(u64_at(40) + (index as u64 / 8192) * 8)?;
                    let other_l2 = (other_l1 & MASK) + (index as u64 % 8192) * 8;
                    let entry = b.read64(other_l2)?;
                    b.set64(other_l2, entry | COPIED)?;
                }
            }
        }
    }
    let l1_position = u64_at(40) + (guest / 8192) * 8;
    let l1 = b.read64(l1_position)?;
    let l2 = if l1 & MASK == 0 || l1 & COPIED == 0 {
        let table = b.append()?;
        if l1 & MASK != 0 {
            let mut prior = vec![0; CLUSTER as usize];
            raw.read_exact_at(l1 & MASK, &mut prior)?;
            *b.cluster(table)? = prior;
            let remaining = b.reference(l1 & MASK, -1)?;
            if remaining == 1 {
                let l1_entries = u32::from_be_bytes(header[36..40].try_into().unwrap()) as u64;
                for index in 0..l1_entries {
                    if index == guest / 8192 {
                        continue;
                    }
                    let position = u64_at(40) + index * 8;
                    let other = b.read64(position)?;
                    if other & MASK == l1 & MASK {
                        b.set64(position, other | COPIED)?;
                    }
                }
            }
        }
        b.reference(table, 1)?;
        b.set64(l1_position, table | COPIED)?;
        table
    } else {
        l1 & MASK
    };
    b.set64(l2 + (guest % 8192) * 8, replacement)?;
    let mut patches = Vec::new();
    for (offset, new) in b.patches {
        let mut old = if offset < b.original {
            vec![0; (b.original - offset).min(CLUSTER) as usize]
        } else {
            Vec::new()
        };
        if !old.is_empty() {
            raw.read_exact_at(offset, &mut old)?;
        }
        if old != new {
            patches.push(Patch { offset, old, new });
        }
    }
    let record = Record {
        original_length: b.original,
        final_length: b.end,
        original_digest: digest,
        patches,
    };
    journal::commit_authorized(&context.path, raw, record, cut, &context.authorized)?;
    Ok(replacement)
}

fn private_l2(b: &mut Builder, l1_position: u64) -> io::Result<u64> {
    let entry = b.read64(l1_position)?;
    if entry & COPIED != 0 {
        return Ok(entry & MASK);
    }
    let table = b.append()?;
    if entry & MASK != 0 {
        let prior = b.cluster(entry & MASK)?.clone();
        *b.cluster(table)? = prior;
        b.reference(entry & MASK, -1)?;
    }
    b.reference(table, 1)?;
    b.set64(l1_position, table | COPIED)?;
    Ok(table)
}

pub(super) fn resize(
    context: &super::WriteContext,
    raw: Arc<RawWriter>,
    mappings: &[u64],
    old_size: u64,
    new_size: u64,
    policy: crate::ShrinkPolicy,
    cut: Option<usize>,
) -> io::Result<Vec<u64>> {
    raw.require_single_link_for_journal()?;
    if new_size < old_size {
        if policy == crate::ShrinkPolicy::Reject {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "shrink requires an explicit tail-removal policy",
            ));
        }
        if policy == crate::ShrinkPolicy::RequireZero {
            let mut bytes = vec![0; CLUSTER as usize];
            let mut offset = new_size;
            while offset < old_size {
                let within = offset % CLUSTER;
                let count = (CLUSTER - within).min(old_size - offset) as usize;
                let descriptor = mappings[(offset / CLUSTER) as usize];
                bytes[..count].fill(0);
                if descriptor & 1 == 0 {
                    if descriptor & MASK != 0 {
                        raw.read_exact_at((descriptor & MASK) + within, &mut bytes[..count])?;
                    } else if let Some(parent) = &context.parent {
                        let covered =
                            parent.len().saturating_sub(offset).min(count as u64) as usize;
                        if covered != 0 {
                            parent.read_exact_at(offset, &mut bytes[..covered])?;
                        }
                    }
                    if bytes[..count].iter().any(|b| *b != 0) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "removed tail contains nonzero content",
                        ));
                    }
                }
                offset += count as u64;
            }
        }
    }
    let mut header = [0; 104];
    raw.read_exact_at(0, &mut header)?;
    let value = |at| u64::from_be_bytes(header[at..at + 8].try_into().unwrap());
    let old_l1 = u32::from_be_bytes(header[36..40].try_into().unwrap()) as u64;
    if old_l1 > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native resize requires a single-cluster L1 table",
        ));
    }
    let source = LockedSource {
        size: raw.len(),
        raw: raw.clone(),
    };
    let digest = journal::digest_reader(&source)?;
    let mut b = Builder {
        raw: raw.clone(),
        original: raw.len(),
        end: raw.len().div_ceil(CLUSTER) * CLUSTER,
        patches: BTreeMap::new(),
        ref_table: value(48),
        ref_table_clusters: u32::from_be_bytes(header[56..60].try_into().unwrap()) as u64,
    };
    if !b.original.is_multiple_of(CLUSTER) {
        b.cluster(b.original / CLUSTER * CLUSTER)?;
    }
    let new_count = new_size.div_ceil(CLUSTER) as usize;
    let old_count = mappings.len();
    let needed_l1 = (new_count as u64).div_ceil(8192);
    let mut l1_offset = value(40);
    if old_l1 == 0 && needed_l1 != 0 {
        l1_offset = b.append()?;
        b.reference(l1_offset, 1)?;
    }
    for index in old_l1..needed_l1 {
        b.set64(l1_offset + index * 8, 0)?;
    }
    let mut next = mappings.to_vec();
    next.resize(new_count, 0);
    if new_count < old_count {
        for descriptor in &mappings[new_count..] {
            if descriptor & MASK != 0 {
                b.reference(descriptor & MASK, -1)?;
            }
        }
        for index in (new_count as u64).div_ceil(8192)..old_l1 {
            let position = l1_offset + index * 8;
            let entry = b.read64(position)?;
            if entry & MASK != 0 {
                b.reference(entry & MASK, -1)?;
                b.set64(position, 0)?;
            }
        }
        if new_count != 0 && !new_count.is_multiple_of(8192) {
            let position = l1_offset + (new_count as u64 / 8192) * 8;
            if b.read64(position)? & MASK != 0 {
                let table = private_l2(&mut b, position)?;
                for index in new_count % 8192..8192 {
                    b.set64(table + index as u64 * 8, 0)?;
                }
            }
        }
    }
    let boundary = old_size.min(new_size);
    if boundary != 0 && !boundary.is_multiple_of(CLUSTER) {
        let index = (boundary / CLUSTER) as usize;
        let descriptor = mappings[index];
        if descriptor & 1 == 0 && (descriptor & MASK != 0 || context.parent.is_some()) {
            let payload = b.append()?;
            let mut bytes = vec![0; CLUSTER as usize];
            if descriptor & MASK != 0 {
                raw.read_exact_at(descriptor & MASK, &mut bytes)?;
            } else if let Some(parent) = &context.parent {
                let offset = index as u64 * CLUSTER;
                let covered = parent.len().saturating_sub(offset).min(boundary % CLUSTER) as usize;
                if covered != 0 {
                    parent.read_exact_at(offset, &mut bytes[..covered])?;
                }
            }
            bytes[(boundary % CLUSTER) as usize..].fill(0);
            *b.cluster(payload)? = bytes;
            b.reference(payload, 1)?;
            if descriptor & MASK != 0 {
                b.reference(descriptor & MASK, -1)?;
            }
            let table = private_l2(&mut b, l1_offset + (index as u64 / 8192) * 8)?;
            b.set64(table + (index as u64 % 8192) * 8, payload | COPIED)?;
            next[index] = payload | COPIED;
        }
    }
    if new_size > old_size && context.parent.is_some() {
        for (index, descriptor) in next.iter_mut().enumerate().skip(old_count) {
            let table = private_l2(&mut b, l1_offset + (index as u64 / 8192) * 8)?;
            b.set64(table + (index as u64 % 8192) * 8, 1)?;
            *descriptor = 1;
        }
    }
    let header = b.cluster(0)?;
    header[24..32].copy_from_slice(&new_size.to_be_bytes());
    header[36..40].copy_from_slice(&(old_l1.max(needed_l1) as u32).to_be_bytes());
    header[40..48].copy_from_slice(&l1_offset.to_be_bytes());
    let mut patches = Vec::new();
    for (offset, new) in b.patches {
        let mut old = if offset < b.original {
            vec![0; (b.original - offset).min(CLUSTER) as usize]
        } else {
            vec![]
        };
        if !old.is_empty() {
            raw.read_exact_at(offset, &mut old)?;
        }
        if old != new {
            patches.push(Patch { offset, old, new });
        }
    }
    let record = Record {
        original_length: b.original,
        final_length: b.end,
        original_digest: digest,
        patches,
    };
    journal::commit_authorized(&context.path, raw, record, cut, &context.authorized)?;
    Ok(next)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::io::{Seek, SeekFrom, Write};
    fn shared_l2_fixture(path: &std::path::Path) {
        let writer = super::super::Qcow2Writer::create(path, CLUSTER).unwrap();
        writer.write_all_at(0, &vec![9; CLUSTER as usize]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let second = CLUSTER * 8192;
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        for (offset, value) in [
            (24, second + CLUSTER),
            (CLUSTER, 4 * CLUSTER),
            (CLUSTER + 8, 4 * CLUSTER),
            (4 * CLUSTER, 5 * CLUSTER),
        ] {
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(&value.to_be_bytes()).unwrap();
        }
        file.seek(SeekFrom::Start(36)).unwrap();
        file.write_all(&2u32.to_be_bytes()).unwrap();
        file.seek(SeekFrom::Start(3 * CLUSTER + 8)).unwrap();
        file.write_all(&2u16.to_be_bytes()).unwrap();
        file.write_all(&2u16.to_be_bytes()).unwrap();
    }

    #[test]
    fn shared_l2_write_and_discard_recover_every_changed_cluster_boundary() {
        let _process_boundary = crate::test_sync::writer_test();
        for discard in [false, true] {
            let patches = if discard { 4 } else { 5 };
            for cut in (0..=7).chain(100..100 + patches) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("shared.qcow2");
                shared_l2_fixture(&path);
                let raw = Arc::new(RawWriter::open(&path).unwrap());
                let context = super::super::WriteContext {
                    path: path.clone(),
                    authorized: vec![],
                    parent: None,
                };
                let mut mappings = vec![0; 8193];
                mappings[0] = 5 * CLUSTER;
                mappings[8192] = 5 * CLUSTER;
                let mutation = if discard {
                    Mutation::Discard
                } else {
                    Mutation::Write {
                        within: 100,
                        data: &[7; 4],
                    }
                };
                assert!(
                    mutate_inner(
                        &context,
                        raw.clone(),
                        0,
                        5 * CLUSTER,
                        &mappings,
                        (mutation, Some(cut))
                    )
                    .is_err()
                );
                drop(raw);
                let writer = super::super::Qcow2Writer::open(&path).unwrap();
                let mut actual = [0; 8];
                writer
                    .read_exact_at(CLUSTER * 8192 + 96, &mut actual)
                    .unwrap();
                assert_eq!(actual, [9; 8]);
                writer.read_exact_at(96, &mut actual).unwrap();
                assert_eq!(
                    actual,
                    if discard {
                        [0; 8]
                    } else {
                        [9, 9, 9, 9, 7, 7, 7, 7]
                    }
                );
                drop(writer);
                crate::Qcow2::open(Arc::new(crate::RawDisk::open(&path).unwrap()))
                    .unwrap()
                    .validate_active_mapping()
                    .unwrap();
            }
        }
    }

    #[test]
    fn backed_discard_recovery_masks_parent_without_deallocating_parent_payload() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in (0..=7).chain(100..102) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("child.qcow2");
            let parent = dir.path().join("base.raw");
            let base = vec![9; 2 * CLUSTER as usize];
            std::fs::write(&parent, &base).unwrap();
            crate::create_qcow2_overlay(&path, &parent, "raw", 2 * CLUSTER).unwrap();
            let writer =
                super::super::Qcow2Writer::open_chain(&path, std::slice::from_ref(&parent))
                    .unwrap();
            writer.write_all_at(0, &vec![7; CLUSTER as usize]).unwrap();
            writer.flush().unwrap();
            drop(writer);
            let raw = Arc::new(RawWriter::open(&path).unwrap());
            let context = super::super::WriteContext {
                path: path.clone(),
                authorized: vec![parent.clone()],
                parent: Some(Arc::new(crate::RawDisk::open(&parent).unwrap())),
            };
            let descriptor = (4 * CLUSTER) | COPIED;
            let mappings = [descriptor, 0];
            assert!(
                mutate_inner(
                    &context,
                    raw.clone(),
                    0,
                    descriptor,
                    &mappings,
                    (Mutation::Discard, Some(cut))
                )
                .is_err()
            );
            drop(raw);
            let writer =
                super::super::Qcow2Writer::open_chain(&path, std::slice::from_ref(&parent))
                    .unwrap();
            let mut actual = [1; 512];
            writer.read_exact_at(0, &mut actual).unwrap();
            assert_eq!(actual, [0; 512]);
            writer.read_exact_at(CLUSTER, &mut actual).unwrap();
            assert_eq!(actual, [9; 512]);
            drop(writer);
            crate::Qcow2::open_chain(&path, std::slice::from_ref(&parent))
                .unwrap()
                .validate_active_mapping()
                .unwrap();
            assert!(std::fs::read(parent).unwrap() == base);
        }
    }

    #[test]
    fn backed_allocation_recovery_requires_authorization_and_preserves_parent() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in (0..=7).chain(100..104) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("child.qcow2");
            let parent = dir.path().join("base.raw");
            let original = vec![9; 131072];
            std::fs::write(&parent, &original).unwrap();
            crate::create_qcow2_overlay(&path, &parent, "raw", 131072).unwrap();
            let raw = Arc::new(RawWriter::open(&path).unwrap());
            let context = super::super::WriteContext {
                path: path.clone(),
                authorized: vec![parent.clone()],
                parent: Some(Arc::new(crate::RawDisk::open(&parent).unwrap())),
            };
            assert!(
                allocate_inner(
                    &context,
                    raw.clone(),
                    0,
                    0,
                    &[0, 0],
                    19,
                    (&[7; 512], Some(cut))
                )
                .is_err()
            );
            drop(raw);
            let before = std::fs::read(&path).unwrap();
            assert!(super::super::Qcow2Writer::open(&path).is_err());
            assert!(std::fs::read(&path).unwrap() == before);
            let writer =
                super::super::Qcow2Writer::open_chain(&path, std::slice::from_ref(&parent))
                    .unwrap();
            let mut actual = [0; 512];
            writer.read_exact_at(19, &mut actual).unwrap();
            assert_eq!(actual, [7; 512]);
            writer.read_exact_at(65536, &mut actual).unwrap();
            assert_eq!(actual, [9; 512]);
            writer.read_exact_at(0, &mut actual[..19]).unwrap();
            assert_eq!(&actual[..19], &[9; 19]);
            drop(writer);
            crate::Qcow2::open_chain(&path, std::slice::from_ref(&parent))
                .unwrap()
                .validate_active_mapping()
                .unwrap();
            assert!(std::fs::read(&parent).unwrap() == original);
        }
    }

    #[test]
    fn interrupted_allocations_reopen_with_exact_ownership_and_payload() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in (0..=7).chain(100..104) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sparse.qcow2");
            drop(super::super::Qcow2Writer::create(&path, 131072).unwrap());
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(65536)).unwrap();
            file.write_all(&0u64.to_be_bytes()).unwrap();
            file.seek(SeekFrom::Start(3 * 65536 + 4 * 2)).unwrap();
            file.write_all(&[0; 6]).unwrap();
            drop(file);
            let raw = Arc::new(RawWriter::open(&path).unwrap());
            let context = super::super::WriteContext {
                path: path.clone(),
                authorized: vec![],
                parent: None,
            };
            assert!(
                allocate_inner(
                    &context,
                    raw.clone(),
                    0,
                    0,
                    &[0, 0],
                    19,
                    (&[7; 512], Some(cut))
                )
                .is_err()
            );
            drop(raw);
            let writer = super::super::Qcow2Writer::open(&path).unwrap();
            let mut bytes = [9; 512];
            writer.read_exact_at(19, &mut bytes).unwrap();
            assert!(bytes == [7; 512], "cut {cut}");
            writer.read_exact_at(65536, &mut bytes).unwrap();
            assert_eq!(bytes, [0; 512]);
            drop(writer);
            let disk = crate::Qcow2::open(Arc::new(crate::RawDisk::open(&path).unwrap())).unwrap();
            disk.validate_active_mapping().unwrap();
        }
    }
    #[test]
    fn resize_recovers_header_mapping_refcount_and_boundary_cow_at_every_cut() {
        let _process_boundary = crate::test_sync::writer_test();
        for empty in [false, true] {
            let patch_count = if empty { 3 } else { 4 };
            for cut in (0..8).chain(100..100 + patch_count) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("resize.qcow2");
                let size = if empty { 0 } else { 2 * CLUSTER };
                let w = super::super::Qcow2Writer::create(&path, size).unwrap();
                if !empty {
                    w.write_all_at(0, &vec![9; size as usize]).unwrap();
                    w.flush().unwrap();
                }
                let mappings = w.mappings.lock().unwrap().clone();
                let result = resize(
                    &w.context,
                    w.raw.clone(),
                    &mappings,
                    size,
                    512,
                    crate::ShrinkPolicy::AllowDataLoss,
                    Some(cut),
                );
                assert!(result.is_err(), "cut {cut} empty {empty}");
                drop(w);
                let mut recovered = super::super::Qcow2Writer::open(&path).unwrap();
                assert_eq!(recovered.len(), 512);
                let mut bytes = [0; 512];
                recovered.read_exact_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, if empty { [0; 512] } else { [9; 512] });
                recovered
                    .resize(CLUSTER, crate::ShrinkPolicy::Reject)
                    .unwrap();
                let mut tail = vec![1; (CLUSTER - 512) as usize];
                recovered.read_exact_at(512, &mut tail).unwrap();
                assert!(tail.iter().all(|b| *b == 0));
            }
        }
    }
    #[test]
    fn shrink_shared_l2_releases_removed_aliases_before_boundary_cow() {
        let _process_boundary = crate::test_sync::writer_test();
        for cut in std::iter::once(None).chain((0..8).chain(100..105).map(Some)) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("shared.qcow2");
            shared_l2_fixture(&path);
            let w = super::super::Qcow2Writer::open(&path).unwrap();
            let mappings = w.mappings.lock().unwrap().clone();
            let result = resize(
                &w.context,
                w.raw.clone(),
                &mappings,
                w.len(),
                512,
                crate::ShrinkPolicy::AllowDataLoss,
                cut,
            );
            assert_eq!(result.is_err(), cut.is_some(), "cut {cut:?}");
            drop(w);
            let mut w = super::super::Qcow2Writer::open(&path).unwrap();
            assert_eq!(w.len(), 512);
            w.resize(CLUSTER, crate::ShrinkPolicy::Reject).unwrap();
            let mut bytes = vec![0; CLUSTER as usize];
            w.read_exact_at(0, &mut bytes).unwrap();
            assert_eq!(&bytes[..512], &[9; 512]);
            assert!(bytes[512..].iter().all(|b| *b == 0));
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod snapshot_resize_recovery_tests {
    use super::*;
    use crate::{Qcow2, Qcow2Writer, ReadAt, ShrinkPolicy};
    use std::path::Path;

    fn fixture(path: &Path, size: u64) -> Vec<u64> {
        let mut writer = Qcow2Writer::create_sparse(path, size).unwrap();
        writer.write_all_at(0, &vec![37; size as usize]).unwrap();
        writer.create_snapshot(b"saved", b"saved").unwrap();
        writer.flush().unwrap();
        let mappings = writer.mappings.lock().unwrap().clone();
        drop(writer);
        mappings
    }

    #[test]
    fn snapshot_resize_recovers_every_fixture_patch_and_persistence_cut() {
        let _guard = crate::test_sync::writer_test();
        for (old_size, new_size, policy) in [
            (CLUSTER + 512, 3 * CLUSTER, ShrinkPolicy::Reject),
            (3 * CLUSTER, CLUSTER + 512, ShrinkPolicy::AllowDataLoss),
        ] {
            let patch_count = {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("probe");
                let mappings = fixture(&path, old_size);
                let before = std::fs::read(&path).unwrap();
                let context = super::super::WriteContext {
                    path: path.clone(),
                    authorized: vec![],
                    parent: None,
                };
                resize(
                    &context,
                    Arc::new(RawWriter::open(&path).unwrap()),
                    &mappings,
                    old_size,
                    new_size,
                    policy,
                    None,
                )
                .unwrap();
                let after = std::fs::read(&path).unwrap();
                let old: Vec<_> = before.chunks(CLUSTER as usize).collect();
                after
                    .chunks(CLUSTER as usize)
                    .enumerate()
                    .filter(|(index, bytes)| old.get(*index).copied().unwrap_or(&[]) != *bytes)
                    .count()
            };
            for cut in (0..=7).chain(100..100 + patch_count) {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("image");
                let mappings = fixture(&path, old_size);
                let context = super::super::WriteContext {
                    path: path.clone(),
                    authorized: vec![],
                    parent: None,
                };
                assert!(
                    resize(
                        &context,
                        Arc::new(RawWriter::open(&path).unwrap()),
                        &mappings,
                        old_size,
                        new_size,
                        policy,
                        Some(cut)
                    )
                    .is_err(),
                    "cut {cut}"
                );
                assert!(Qcow2::open_chain(&path, &[]).is_err());
                drop(Qcow2Writer::open(&path).unwrap());
                let disk = Arc::new(Qcow2::open_chain(&path, &[]).unwrap());
                disk.validate_active_mapping().unwrap();
                assert_eq!(disk.len(), new_size);
                let mut expected = vec![37; old_size as usize];
                expected.resize(new_size as usize, 0);
                let mut bytes = vec![0; new_size as usize];
                disk.read_exact_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, expected, "cut {cut}");
                let saved = disk.open_snapshot(b"saved").unwrap();
                assert_eq!(saved.len(), old_size);
                bytes.resize(old_size as usize, 0);
                saved.read_exact_at(0, &mut bytes).unwrap();
                assert!(bytes.iter().all(|byte| *byte == 37), "cut {cut}");
            }
        }
    }
    fn backed_fixture(dir: &Path, format: crate::ImageFormat, old_size: u64) -> Qcow2Writer {
        let parent = dir.join("parent");
        let child = dir.join("child");
        let writer = crate::ImageWriter::create(&parent, format, 3 * CLUSTER).unwrap();
        crate::WriteAt::write_all_at(&writer, 0, &vec![37; (3 * CLUSTER) as usize]).unwrap();
        crate::WriteAt::flush(&writer).unwrap();
        drop(writer);
        crate::create_qcow2_overlay(
            &child,
            &parent,
            if format == crate::ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            },
            old_size,
        )
        .unwrap();
        let mut writer = Qcow2Writer::open_chain(&child, &[parent]).unwrap();
        writer.create_snapshot(b"saved", b"saved").unwrap();
        writer
    }

    #[test]
    fn backed_resize_recovers_growth_masks_and_inherited_boundary_at_every_cut() {
        let _guard = crate::test_sync::writer_test();
        for format in [crate::ImageFormat::Raw, crate::ImageFormat::Qcow2] {
            for (old_size, new_size, policy) in [
                (CLUSTER + 512, 3 * CLUSTER + 512, ShrinkPolicy::Reject),
                (3 * CLUSTER, 512, ShrinkPolicy::AllowDataLoss),
            ] {
                let patch_count = {
                    let dir = tempfile::tempdir().unwrap();
                    let mut writer = backed_fixture(dir.path(), format, old_size);
                    let before = std::fs::read(dir.path().join("child")).unwrap();
                    writer.resize(new_size, policy).unwrap();
                    let after = std::fs::read(dir.path().join("child")).unwrap();
                    let old: Vec<_> = before.chunks(CLUSTER as usize).collect();
                    after
                        .chunks(CLUSTER as usize)
                        .enumerate()
                        .filter(|(index, bytes)| old.get(*index).copied().unwrap_or(&[]) != *bytes)
                        .count()
                };
                for cut in (0..=7).chain(100..100 + patch_count) {
                    let dir = tempfile::tempdir().unwrap();
                    let writer = backed_fixture(dir.path(), format, old_size);
                    let child = dir.path().join("child");
                    let parent = dir.path().join("parent");
                    let parent_bytes = std::fs::read(&parent).unwrap();
                    let mappings = writer.mappings.lock().unwrap().clone();
                    assert!(
                        resize(
                            &writer.context,
                            writer.raw.clone(),
                            &mappings,
                            old_size,
                            new_size,
                            policy,
                            Some(cut)
                        )
                        .is_err(),
                        "cut {cut}"
                    );
                    drop(writer);
                    let interrupted = std::fs::read(&child).unwrap();
                    assert!(Qcow2Writer::open(&child).is_err());
                    assert_eq!(std::fs::read(&child).unwrap(), interrupted);
                    drop(Qcow2Writer::open_chain(&child, std::slice::from_ref(&parent)).unwrap());
                    let disk =
                        Arc::new(Qcow2::open_chain(&child, std::slice::from_ref(&parent)).unwrap());
                    disk.validate_active_mapping().unwrap();
                    assert_eq!(disk.len(), new_size);
                    let mut expected = vec![37; old_size as usize];
                    expected.resize(new_size as usize, 0);
                    let mut bytes = vec![0; new_size as usize];
                    disk.read_exact_at(0, &mut bytes).unwrap();
                    assert_eq!(bytes, expected, "cut {cut}");
                    let saved = disk.open_snapshot(b"saved").unwrap();
                    assert_eq!(saved.len(), old_size);
                    bytes.resize(old_size as usize, 0);
                    saved.read_exact_at(0, &mut bytes).unwrap();
                    assert!(bytes.iter().all(|byte| *byte == 37), "cut {cut}");
                    assert_eq!(std::fs::read(&parent).unwrap(), parent_bytes);
                }
            }
        }
    }
}
