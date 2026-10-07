//! Bounded, fail-closed QCOW2 active-image and authorized backing-chain reader.
use crate::{RawDisk, ReadAt, check_range};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

mod validation;
pub use validation::Qcow2Validation;

const OFFSET_MASK: u64 = 0x00ff_ffff_ffff_fe00;
const COPIED: u64 = 1 << 63;
const COMPRESSED: u64 = 1 << 62;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
fn be64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// Read-only QCOW2 v2/v3 active-image mapping.
///
/// Supports allocated, sparse, explicit-zero, deflate, and zstd clusters.
/// Backing files require explicit authorization through [`Self::open_chain`].
/// Encryption, external data, and extended L2 entries fail closed.
/// Opening parses headers and checks read bounds. Call [`Self::validate_active_mapping`]
/// before capture to reconstruct exact refcounts and check active metadata
/// ownership across the supported chain. Unsupported metadata owners fail closed.
/// The caller must retain immutable sources. Embedded paths are only opened
/// after explicit authorization through [`Self::open_chain`].
pub struct Qcow2 {
    source: Arc<dyn ReadAt>,
    entry_cache: Mutex<HashMap<u64, (u64, crate::CacheReservation)>>,
    version: u32,
    size: u64,
    cluster_size: u64,
    l1_offset: u64,
    l1_size: u64,
    compression: u8,
    backing_name: Option<String>,
    backing_format: Option<String>,
    backing: Option<Arc<dyn ReadAt>>,
    backing_qcow: Option<Arc<Qcow2>>,
    refcount_offset: u64,
    refcount_clusters: u64,
    refcount_order: u32,
    snapshots: u32,
    extra_metadata: bool,
}

impl Qcow2 {
    /// Parse a bounded header and retain its container reader.
    ///
    /// Embedded backing paths are never opened by this constructor. Images
    /// declaring a backing file require [`Self::open_chain`].
    pub fn open(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        Self::open_with_limits(source, crate::ParserLimits::default())
    }

    /// Open an unbacked image with caller-tightened parser ceilings.
    pub fn open_with_limits(
        source: Arc<dyn ReadAt>,
        limits: crate::ParserLimits,
    ) -> io::Result<Self> {
        let budget = crate::ReadBudget::new(limits)?;
        let context = source.context();
        let disk =
            Self::parse(budget.reader(source)).map_err(|e| context.error("parse QCOW2", e))?;
        if disk.backing_name.is_some() {
            return Err(unsupported(
                "QCOW2 backing file requires explicit authorization",
            ));
        }
        Ok(disk)
    }

    fn parse(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        let mut header = [0; 104];
        source.read_exact_at(0, &mut header[..72])?;
        if &header[..4] != b"QFI\xfb" {
            return Err(invalid("invalid QCOW2 magic"));
        }
        let version = be32(&header[4..8]);
        if !matches!(version, 2 | 3) {
            return Err(unsupported("QCOW2 version is not 2 or 3"));
        }
        let backing_offset = be64(&header[8..16]);
        let backing_length = u64::from(be32(&header[16..20]));
        let bits = be32(&header[20..24]);
        if !(9..=21).contains(&bits) {
            return Err(unsupported(
                "QCOW2 cluster size is outside 512 bytes to 2 MiB",
            ));
        }
        let cluster_size = 1u64 << bits;
        if be32(&header[32..36]) != 0 {
            return Err(unsupported("encrypted QCOW2 images are not implemented"));
        }
        let mut header_length = 72;
        let mut compression = 0;
        if version == 3 {
            source.read_exact_at(72, &mut header[72..])?;
            let incompatible = be64(&header[72..80]);
            if incompatible & !8 != 0 {
                return Err(unsupported(
                    "dirty, corrupt, or incompatible QCOW2 feature bits",
                ));
            }
            if be64(&header[88..96]) & 2 != 0 {
                return Err(unsupported("QCOW2 external raw data is not implemented"));
            }
            if be32(&header[96..100]) > 6 {
                return Err(invalid("invalid QCOW2 refcount order"));
            }
            let length = u64::from(be32(&header[100..104]));
            header_length = length;
            if length < 104 || length > cluster_size || length % 8 != 0 {
                return Err(invalid("invalid QCOW2 header length"));
            }
            check_range(0, length, source.len())?;
            if length > 104 {
                let mut value = [0];
                source.read_exact_at(104, &mut value)?;
                compression = value[0];
            }
            if (compression != 0) != (incompatible & 8 != 0) {
                return Err(invalid("inconsistent QCOW2 compression feature"));
            }
            if compression > 1 {
                return Err(unsupported("unknown QCOW2 compression type"));
            }
        }
        let (backing_name, backing_format, extra_metadata) = Self::backing_header(
            &*source,
            header_length,
            cluster_size,
            backing_offset,
            backing_length,
        )?;
        let size = be64(&header[24..32]);
        let l1_size = u64::from(be32(&header[36..40]));
        let l1_offset = be64(&header[40..48]);
        if l1_size > (32 * 1024 * 1024 / 8) {
            return Err(unsupported("QCOW2 L1 table exceeds 32 MiB limit"));
        }
        let coverage = cluster_size * (cluster_size / 8);
        if l1_size < size.div_ceil(coverage) {
            return Err(invalid("QCOW2 L1 table does not cover virtual size"));
        }
        if l1_size != 0 {
            Self::cluster_range(&*source, l1_offset, l1_size * 8, cluster_size)?;
        }
        let refcount_offset = be64(&header[48..56]);
        let refcount_clusters = u64::from(be32(&header[56..60]));
        if refcount_clusters == 0 {
            return Err(invalid("QCOW2 has no refcount table"));
        }
        Self::cluster_range(
            &*source,
            refcount_offset,
            refcount_clusters * cluster_size,
            cluster_size,
        )?;
        Ok(Self {
            source,
            entry_cache: Mutex::new(HashMap::new()),
            version,
            size,
            cluster_size,
            l1_offset,
            l1_size,
            compression,
            backing_name,
            backing_format,
            backing: None,
            backing_qcow: None,
            refcount_offset,
            refcount_clusters,
            refcount_order: if version == 3 {
                be32(&header[96..100])
            } else {
                4
            },
            snapshots: be32(&header[60..64]),
            extra_metadata,
        })
    }

    /// Open an active QCOW2 image and only its explicitly authorized backing files.
    ///
    /// Paths embedded in the image are resolved relative to its parent, then
    /// canonicalized and checked against `authorized_backing_paths` before opening.
    /// Protocol URLs are rejected. Chains are limited to 32 images; repeated
    /// canonical paths (and repeated device/inode identities on Unix) are rejected.
    /// The retained read-only handles do not provide snapshots: every source must
    /// remain immutable for the full capture and deferred WIM write.
    pub fn open_chain(
        path: impl AsRef<Path>,
        authorized_backing_paths: &[PathBuf],
    ) -> io::Result<Self> {
        Self::open_chain_with_limits(
            path,
            authorized_backing_paths,
            crate::ParserLimits::default(),
        )
    }

    /// Open an explicitly authorized backing chain with a shared parser budget.
    pub fn open_chain_with_limits(
        path: impl AsRef<Path>,
        authorized_backing_paths: &[PathBuf],
        limits: crate::ParserLimits,
    ) -> io::Result<Self> {
        let budget = crate::ReadBudget::new(limits)?;
        let approved: HashSet<_> = authorized_backing_paths
            .iter()
            .map(|path| {
                budget.work(1)?;
                budget.metadata(
                    (path.as_os_str().as_encoded_bytes().len() + std::mem::size_of::<PathBuf>())
                        as u64
                        * 2,
                )?;
                std::fs::canonicalize(path).map_err(|e| {
                    crate::ReadContext {
                        container: Some(path.clone()),
                        ..Default::default()
                    }
                    .error("authorize QCOW2 backing path", e)
                })
            })
            .collect::<io::Result<_>>()?;
        let mut paths = HashSet::new();
        let mut identities = HashSet::new();
        Self::open_chain_inner(
            &std::fs::canonicalize(path)?,
            &approved,
            &mut paths,
            &mut identities,
            0,
            &budget,
        )
    }

    fn open_chain_inner(
        path: &Path,
        approved: &HashSet<PathBuf>,
        paths: &mut HashSet<PathBuf>,
        identities: &mut HashSet<(u64, u64)>,
        depth: usize,
        budget: &crate::ReadBudget,
    ) -> io::Result<Self> {
        Self::open_chain_inner_unannotated(path, approved, paths, identities, depth, budget)
            .map_err(|e| {
                crate::ReadContext {
                    container: Some(path.to_path_buf()),
                    ..Default::default()
                }
                .error("open QCOW2 backing chain", e)
            })
    }

    fn open_chain_inner_unannotated(
        path: &Path,
        approved: &HashSet<PathBuf>,
        paths: &mut HashSet<PathBuf>,
        identities: &mut HashSet<(u64, u64)>,
        depth: usize,
        budget: &crate::ReadBudget,
    ) -> io::Result<Self> {
        if depth as u64 >= budget.limits().recursion_depth.min(32)
            || !paths.insert(path.to_path_buf())
        {
            return Err(invalid("QCOW2 backing cycle or depth limit exceeded"));
        }
        let source = Arc::new(RawDisk::open(path)?);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = source
                .file
                .lock()
                .map_err(|_| io::Error::other("disk reader mutex poisoned"))?
                .metadata()?;
            if !identities.insert((metadata.dev(), metadata.ino())) {
                return Err(invalid("QCOW2 backing chain repeats a file identity"));
            }
        }
        let context = source.context();
        let mut disk = Self::parse(budget.reader(source))
            .map_err(|e| context.error("parse QCOW2 backing container", e))?;
        if let Some(name) = &disk.backing_name {
            // QCOW2 also allows URI protocols. Never pass these to a filesystem
            // resolver, including Windows alternate-data-stream syntax.
            let drive_path = cfg!(windows)
                && name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && name.as_bytes().get(1) == Some(&b':')
                && !name[2..].contains(':')
                && Path::new(name).is_absolute();
            if name.contains(':') && !drive_path {
                return Err(unsupported("QCOW2 backing protocol or alternate stream"));
            }
            let requested = path
                .parent()
                .ok_or_else(|| invalid("QCOW2 image has no parent"))?
                .join(name);
            let backing_path = std::fs::canonicalize(requested)?;
            if !approved.contains(&backing_path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "QCOW2 backing path is not explicitly authorized",
                ));
            }
            let raw = Arc::new(RawDisk::open(&backing_path)?);
            let mut magic = [0; 4];
            if raw.len() >= 4 {
                raw.read_exact_at(0, &mut magic)?;
            }
            let format = disk
                .backing_format
                .as_deref()
                .unwrap_or(if magic == *b"QFI\xfb" { "qcow2" } else { "raw" });
            disk.backing = Some(match format {
                "qcow2" => {
                    let parent = Arc::new(Self::open_chain_inner(
                        &backing_path,
                        approved,
                        paths,
                        identities,
                        depth + 1,
                        budget,
                    )?);
                    disk.backing_qcow = Some(parent.clone());
                    parent
                }
                "raw" => {
                    if paths.contains(&backing_path) {
                        return Err(invalid("QCOW2 backing cycle"));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        let metadata = raw
                            .file
                            .lock()
                            .map_err(|_| io::Error::other("disk reader mutex poisoned"))?
                            .metadata()?;
                        if !identities.insert((metadata.dev(), metadata.ino())) {
                            return Err(invalid("QCOW2 backing chain repeats a file identity"));
                        }
                    }
                    budget.reader(raw)
                }
                _ => return Err(unsupported("unsupported QCOW2 backing format")),
            });
        }
        Ok(disk)
    }

    fn backing_header(
        source: &dyn ReadAt,
        header_length: u64,
        cluster: u64,
        offset: u64,
        length: u64,
    ) -> io::Result<(Option<String>, Option<String>, bool)> {
        let name = if offset != 0 {
            if length == 0
                || length > 1023
                || offset < header_length
                || offset.checked_add(length).is_none_or(|end| end > cluster)
            {
                return Err(invalid("invalid QCOW2 backing filename range"));
            }
            if let Some(budget) = source.budget() {
                budget.metadata(length)?;
            }
            let mut bytes = vec![0; length as usize];
            source.read_exact_at(offset, &mut bytes)?;
            if bytes.contains(&0) {
                return Err(invalid("QCOW2 backing filename contains NUL"));
            }
            Some(
                String::from_utf8(bytes)
                    .map_err(|_| unsupported("non-UTF8 QCOW2 backing filename"))?,
            )
        } else {
            None
        };
        let end = if offset == 0 { cluster } else { offset };
        let mut cursor = header_length;
        let mut format = None;
        let mut extra_metadata = false;
        while cursor + 8 <= end {
            let mut header = [0; 8];
            source.read_exact_at(cursor, &mut header)?;
            let kind = be32(&header[..4]);
            let length = u64::from(be32(&header[4..]));
            if kind == 0 {
                if length != 0 {
                    return Err(invalid("QCOW2 extension terminator has data"));
                }
                break;
            }
            let padded = length
                .checked_add(7)
                .ok_or_else(|| invalid("QCOW2 extension length overflow"))?
                & !7;
            if cursor + 8 + padded > end {
                return Err(invalid("QCOW2 header extension exceeds header cluster"));
            }
            if !matches!(kind, 0xe2792aca | 0x6803f857) {
                extra_metadata = true;
            }
            if kind == 0xe2792aca {
                if format.is_some() || length == 0 || length > 32 {
                    return Err(invalid("invalid QCOW2 backing format extension"));
                }
                let mut bytes = vec![0; length as usize];
                source.read_exact_at(cursor + 8, &mut bytes)?;
                let value = String::from_utf8(bytes)
                    .map_err(|_| invalid("invalid QCOW2 backing format"))?;
                if !matches!(value.as_str(), "raw" | "qcow2") {
                    return Err(unsupported("unsupported QCOW2 backing format"));
                }
                format = Some(value);
            }
            cursor += 8 + padded;
        }
        if name.is_none() && format.is_some() {
            return Err(invalid("backing format without backing filename"));
        }
        Ok((name, format, extra_metadata))
    }

    fn decompress(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        if let Some(budget) = self.source.budget() {
            budget.decode(self.cluster_size)?;
            if length as u64 > budget.limits().decompression_buffer_bytes {
                return Err(unsupported(
                    "compressed input exceeds configured decompression-buffer limit",
                ));
            }
        }
        let mut input = vec![0; length];
        self.source.read_exact_at(offset, &mut input)?;
        let mut output = vec![0; self.cluster_size as usize];
        if self.compression == 0 {
            let mut decoder = flate2::Decompress::new(false);
            let status = decoder
                .decompress(&input, &mut output, flate2::FlushDecompress::Finish)
                .map_err(|_| invalid("invalid QCOW2 deflate cluster"))?;
            if status != flate2::Status::StreamEnd || decoder.total_out() != self.cluster_size {
                return Err(invalid(
                    "QCOW2 deflate cluster has incorrect decoded length",
                ));
            }
        } else {
            let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                input.as_slice(),
                self.cluster_size,
            )
            .map_err(|_| invalid("invalid QCOW2 zstd frame or excessive window"))?;
            decoder.read_exact(&mut output)?;
            if decoder.read(&mut [0])? != 0 {
                return Err(invalid("QCOW2 zstd cluster exceeds cluster size"));
            }
            let frame = decoder.into_frame_decoder();
            if frame.content_size() != 0 && frame.content_size() != self.cluster_size {
                return Err(invalid("QCOW2 zstd declared content size is incorrect"));
            }
            if let Some(checksum) = frame.get_checksum_from_data()
                && frame.get_calculated_checksum() != Some(checksum)
            {
                return Err(invalid("QCOW2 zstd checksum mismatch"));
            }
        }
        Ok(output)
    }

    fn cluster_range(
        source: &dyn ReadAt,
        offset: u64,
        length: u64,
        cluster: u64,
    ) -> io::Result<()> {
        if offset == 0 || !offset.is_multiple_of(cluster) {
            return Err(invalid("QCOW2 cluster pointer is zero or misaligned"));
        }
        check_range(offset, length, source.len())
    }

    fn entry(&self, offset: u64) -> io::Result<u64> {
        let mut cache = self
            .entry_cache
            .lock()
            .map_err(|_| io::Error::other("QCOW2 entry cache poisoned"))?;
        if let Some((entry, _)) = cache.get(&offset) {
            return Ok(*entry);
        }
        if cache.len() >= 1_048_576 {
            return Err(unsupported("QCOW2 entry cache exceeds bounded entry count"));
        }
        let budget = self
            .source
            .budget()
            .ok_or_else(|| invalid("QCOW2 reader lacks parser budget"))?;
        // Charge conservative HashMap/node/lease overhead before insertion. Each
        // entry is immutable for the retained source's entire reader lifetime.
        let reservation = budget.cache(128)?;
        budget.metadata(128)?;
        let mut entry = [0; 8];
        self.source.read_exact_at(offset, &mut entry)?;
        let entry = be64(&entry);
        // Retain only validated descriptors. Cached words never bypass the
        // normal mapping checks, nor the independent active ownership audit.
        if offset >= self.l1_offset && offset < self.l1_offset + self.l1_size * 8 {
            if entry & !(OFFSET_MASK | COPIED) != 0 {
                return Err(invalid("QCOW2 L1 reserved bits are set"));
            }
            let pointer = entry & OFFSET_MASK;
            if pointer == 0 {
                if entry != 0 {
                    return Err(invalid("QCOW2 unallocated L1 entry has copied flag"));
                }
            } else {
                Self::cluster_range(&*self.source, pointer, self.cluster_size, self.cluster_size)?;
            }
        } else {
            self.mapping_descriptor(entry)?;
        }
        cache.insert(offset, (entry, reservation));
        Ok(entry)
    }

    fn mapping(&self, offset: u64) -> io::Result<Mapping> {
        let cluster = offset / self.cluster_size;
        let entries = self.cluster_size / 8;
        let l1_index = cluster / entries;
        if l1_index >= self.l1_size {
            return Err(invalid("QCOW2 L1 index exceeds table"));
        }
        let l1 = self.entry(self.l1_offset + l1_index * 8)?;
        if l1 & !(OFFSET_MASK | COPIED) != 0 {
            return Err(invalid("QCOW2 L1 reserved bits are set"));
        }
        let l2_offset = l1 & OFFSET_MASK;
        if l2_offset == 0 {
            if l1 != 0 {
                return Err(invalid("QCOW2 unallocated L1 entry has copied flag"));
            }
            return Ok(Mapping::Backing);
        }
        Self::cluster_range(
            &*self.source,
            l2_offset,
            self.cluster_size,
            self.cluster_size,
        )?;
        let l2 = self.entry(l2_offset + (cluster % entries) * 8)?;
        self.mapping_descriptor(l2)
    }

    fn mapping_descriptor(&self, l2: u64) -> io::Result<Mapping> {
        if l2 & COMPRESSED != 0 {
            if l2 & COPIED != 0 {
                return Err(invalid("QCOW2 compressed cluster has copied flag"));
            }
            let shift = 62 - (self.cluster_size.trailing_zeros() - 8);
            let host = l2 & ((1u64 << shift) - 1);
            if host == 0 || host >> 56 != 0 {
                return Err(invalid("invalid QCOW2 compressed offset"));
            }
            let sectors = ((l2 & !(COMPRESSED | COPIED)) >> shift) + 1;
            let length = sectors * 512 - (host % 512);
            check_range(host, length, self.source.len())?;
            return Ok(Mapping::Compressed(host, length as usize));
        }
        let allowed = OFFSET_MASK | COPIED | u64::from(self.version == 3);
        if l2 & !allowed != 0 {
            return Err(invalid("QCOW2 L2 reserved bits are set"));
        }
        let host = l2 & OFFSET_MASK;
        if host != 0 {
            Self::cluster_range(&*self.source, host, self.cluster_size, self.cluster_size)?;
        } else if l2 & COPIED != 0 {
            return Err(invalid("QCOW2 zero host offset has copied flag"));
        }
        if l2 & 1 != 0 {
            Ok(Mapping::Zero)
        } else if host == 0 {
            Ok(Mapping::Backing)
        } else {
            Ok(Mapping::Allocated(host))
        }
    }
}

enum Mapping {
    Allocated(u64),
    Zero,
    Backing,
    Compressed(u64, usize),
}

impl ReadAt for Qcow2 {
    fn context(&self) -> crate::ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<crate::ReadBudget> {
        self.source.budget()
    }
    fn len(&self) -> u64 {
        self.size
    }

    fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let mut context = self.context();
        context.offset = Some(offset);
        (move || {
            let mut offset = offset;
            let mut destination = destination;
            check_range(offset, destination.len() as u64, self.size)?;
            while !destination.is_empty() {
                let within = offset % self.cluster_size;
                let count = destination.len().min((self.cluster_size - within) as usize);
                let (part, remainder) = destination.split_at_mut(count);
                match self.mapping(offset)? {
                    Mapping::Allocated(host) => self.source.read_exact_at(host + within, part)?,
                    Mapping::Zero => {
                        if let Some(budget) = self.budget() {
                            budget.work(1)?;
                        }
                        part.fill(0);
                    }
                    Mapping::Backing => {
                        let covered = self.backing.as_ref().map_or(0, |backing| {
                            backing.len().saturating_sub(offset).min(count as u64)
                        });
                        if covered < count as u64
                            && let Some(budget) = self.budget()
                        {
                            budget.work(1)?;
                        }
                        part.fill(0);
                        if let Some(backing) = &self.backing
                            && offset < backing.len()
                        {
                            let covered = (backing.len() - offset).min(count as u64) as usize;
                            backing.read_exact_at(offset, &mut part[..covered])?;
                        }
                    }
                    Mapping::Compressed(host, length) => {
                        let decoded = self.decompress(host, length)?;
                        part.copy_from_slice(&decoded[within as usize..within as usize + count]);
                    }
                }
                offset += count as u64;
                destination = remainder;
            }
            Ok(())
        })()
        .map_err(|e| context.error("read QCOW2 virtual disk", e))
    }
}
