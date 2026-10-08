//! Retained split hosted sparse extents with bounded allocation and pinned parents.
use crate::{
    CacheReservation, ParserLimits, RawWriter, ReadAt, ReadBudget, Vmdk,
    transaction::{self, Patch, Record},
    transaction_set::{self, File},
    vmdk::PinnedParentGraph,
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid writable split sparse VMDK",
    )
}
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "split sparse request exceeds supported metadata, geometry or resource bounds",
    )
}
struct Source {
    raw: Arc<RawWriter>,
    size: u64,
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, bytes.len() as u64, self.size)?;
        self.raw.read_exact_at(offset, bytes)
    }
}
struct Spec {
    name: String,
    size: u64,
}
struct Descriptor {
    specs: Vec<Spec>,
    cid_offset: u64,
    cid_width: usize,
    size: u64,
}
fn descriptor(source: &dyn ReadAt) -> io::Result<Descriptor> {
    descriptor_mode(source, false)
}
fn descriptor_mode(source: &dyn ReadAt, allow_parent: bool) -> io::Result<Descriptor> {
    if source.is_empty() || source.len() > 65536 {
        return Err(unsupported());
    }
    let mut bytes = vec![0; source.len() as usize];
    source.read_exact_at(0, &mut bytes)?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(invalid());
    }
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| invalid())?;
    let mut version = false;
    let mut parent = false;
    let mut hint = false;
    let mut profile = false;
    let mut cid = None;
    let mut specs = Vec::new();
    let mut cursor = 0;
    for original in text.split_inclusive('\n') {
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            cursor += original.len();
            continue;
        }
        if let Some((key, value)) = original.split_once('=') {
            let value = value.trim();
            match key.trim() {
                "version" if !version && value == "1" => version = true,
                "parentCID"
                    if !parent
                        && (value == "ffffffff"
                            || (allow_parent
                                && value.len() == 8
                                && value.bytes().all(|byte| byte.is_ascii_hexdigit()))) =>
                {
                    parent = true
                }
                "parentFileNameHint"
                    if allow_parent && !hint && value.starts_with('"') && value.ends_with('"') =>
                {
                    hint = true
                }
                "createType" if !profile && value == "\"twoGbMaxExtentSparse\"" => profile = true,
                "CID"
                    if cid.is_none()
                        && !value.is_empty()
                        && value.len() <= 8
                        && value.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    let equals = original.find('=').ok_or_else(invalid)?;
                    let whitespace =
                        original[equals + 1..].len() - original[equals + 1..].trim_start().len();
                    cid = Some(((cursor + equals + 1 + whitespace) as u64, value.len()));
                }
                key if key.starts_with("ddb.") => {}
                _ => return Err(unsupported()),
            }
        } else {
            let quote = line.find('"').ok_or_else(invalid)?;
            let close = line[quote + 1..].find('"').ok_or_else(invalid)? + quote + 1;
            let fields = line[..quote].split_whitespace().collect::<Vec<_>>();
            if fields.len() != 3
                || fields[0] != "RW"
                || fields[2] != "SPARSE"
                || !line[close + 1..].trim().is_empty()
                || specs.len() >= 256
            {
                return Err(unsupported());
            }
            let size = fields[1]
                .parse::<u64>()
                .ok()
                .and_then(|n| n.checked_mul(512))
                .ok_or_else(invalid)?;
            let name = &line[quote + 1..close];
            if size == 0
                || size > 2 * 1024 * 1024 * 1024
                || name.is_empty()
                || name.contains([':', '\\'])
                || name.chars().any(char::is_control)
            {
                return Err(unsupported());
            }
            specs.push(Spec {
                name: name.to_owned(),
                size,
            });
        }
        cursor += original.len();
    }
    if !version || !parent || !profile || specs.is_empty() {
        return Err(unsupported());
    }
    let (cid_offset, cid_width) = cid.ok_or_else(invalid)?;
    let size = specs.iter().try_fold(0u64, |sum, spec| {
        sum.checked_add(spec.size).ok_or_else(invalid)
    })?;
    if size > 32 * 1024 * 1024 * 1024 {
        return Err(unsupported());
    }
    Ok(Descriptor {
        specs,
        cid_offset,
        cid_width,
        size,
    })
}
struct Extent {
    start: u64,
    size: u64,
    redundant_required: bool,
    primary_directory: u64,
    redundant_directory: u64,
    protected: Vec<(u64, u64)>,
}
#[derive(Clone, Copy)]
struct TablePlan {
    table: usize,
    padding: Option<u64>,
}
#[derive(Clone, Copy)]
struct ArenaPlan {
    offset: u64,
    length: u64,
    prepare: bool,
}
struct RequestPlan {
    tables: Vec<Vec<TablePlan>>,
    arenas: Vec<Option<ArenaPlan>>,
    _cache: CacheReservation,
}
struct State {
    epoch: bool,
    failed: bool,
    mappings: Vec<Vec<u64>>,
    zero_mask: Vec<Vec<bool>>,
    primary: Vec<Vec<Option<u64>>>,
    redundant: Vec<Vec<Option<u64>>>,
    overhead: Vec<u64>,
    validation_work: u64,
    validation_metadata: u64,
}
pub(super) struct Sparse {
    parent: Option<PinnedParentGraph>,
    files: Vec<File>,
    extents: Vec<Extent>,
    size: u64,
    cid_offset: u64,
    cid_width: usize,
    state: Mutex<State>,
    _cache: CacheReservation,
    budget: ReadBudget,
}
pub(super) struct Opened {
    pub(super) raw: Arc<RawWriter>,
    pub(super) identity: same_file::Handle,
    pub(super) path: PathBuf,
    pub(super) sparse: Sparse,
}
pub(super) fn matches_profile(path: &Path) -> io::Result<bool> {
    let source = crate::RawDisk::open(path)?;
    if source.len() == 0 || source.len() > 65536 {
        return Ok(false);
    }
    let mut bytes = vec![0; source.len() as usize];
    source.read_exact_at(0, &mut bytes)?;
    Ok(std::str::from_utf8(&bytes).is_ok_and(|text| {
        text.lines().any(|line| {
            line.split_once('=').is_some_and(|(key, value)| {
                key.trim() == "createType" && value.trim() == "\"twoGbMaxExtentSparse\""
            })
        })
    }))
}
#[cfg(test)]
fn validate_sources(
    sources: &[Arc<dyn ReadAt>],
    budget: &ReadBudget,
    files: &[File],
) -> io::Result<()> {
    validate_sources_parented(sources, budget, files, None)
}
fn validate_sources_parented(
    sources: &[Arc<dyn ReadAt>],
    budget: &ReadBudget,
    files: &[File],
    parent: Option<&PinnedParentGraph>,
) -> io::Result<()> {
    let source = budget.reader(sources.first().ok_or_else(invalid)?.clone());
    budget.metadata(source.len())?;
    let desc = if parent.is_some() {
        descriptor_mode(&*source, true)?
    } else {
        descriptor(&*source)?
    };
    if sources.len() != desc.specs.len() + 1 || sources.len() != files.len() {
        return Err(invalid());
    }
    for ((spec, source), file) in desc.specs.iter().zip(&sources[1..]).zip(&files[1..]) {
        budget.work(1)?;
        if files[0]
            .path
            .parent()
            .ok_or_else(invalid)?
            .join(&spec.name)
            .canonicalize()?
            != file.path
        {
            return Err(invalid());
        }
        let disk = Vmdk::open_with_budget(source.clone(), budget)?;
        if disk.len() != spec.size {
            return Err(invalid());
        }
        disk.writer_mappings()?;
        let mut header = [0; 512];
        budget.work(1)?;
        source.read_exact_at(0, &mut header)?;
        if u64::from_le_bytes(header[20..28].try_into().unwrap()) != 128
            || u32::from_le_bytes(header[44..48].try_into().unwrap()) != 512
            || u64::from_le_bytes(header[36..44].try_into().unwrap()) > 2048
        {
            return Err(unsupported());
        }
    }
    if let Some(parent) = parent {
        let paths = files[1..]
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        parent.validate_child(source, &files[0].path, &sources[1..], &paths)?;
    }
    Ok(())
}
struct Slots {
    primary: Vec<Option<u64>>,
    redundant: Vec<Option<u64>>,
    redundant_required: bool,
    primary_directory: u64,
    redundant_directory: u64,
    overhead: u64,
    protected: Vec<(u64, u64)>,
}
fn mapping_slots(source: &dyn ReadAt, size: u64, budget: &ReadBudget) -> io::Result<Slots> {
    let mut header = [0; 512];
    budget.work(1)?;
    source.read_exact_at(0, &mut header)?;
    let primary_directory = u64::from_le_bytes(header[56..64].try_into().unwrap())
        .checked_mul(512)
        .ok_or_else(invalid)?;
    let redundant_directory = u64::from_le_bytes(header[48..56].try_into().unwrap())
        .checked_mul(512)
        .ok_or_else(invalid)?;
    let overhead = u64::from_le_bytes(header[64..72].try_into().unwrap()) * 512;
    let mut protected = vec![(0, 512)];
    let desc = u64::from_le_bytes(header[28..36].try_into().unwrap()) * 512;
    let desc_len = u64::from_le_bytes(header[36..44].try_into().unwrap()) * 512;
    if desc_len != 0 {
        protected.push((desc, desc + desc_len));
    }
    let count = size.div_ceil(65536);
    let directory_bytes = (count.div_ceil(512) * 4).div_ceil(512) * 512;
    for directory in [primary_directory, redundant_directory] {
        if directory != 0 {
            protected.push((directory, directory + directory_bytes));
        }
    }

    budget.metadata(count * 32)?;
    let mut primary = Vec::with_capacity(count as usize);
    let mut redundant = Vec::with_capacity(count as usize);
    for table in 0..count.div_ceil(512) {
        let mut entry = [0; 4];
        budget.work(1)?;
        source.read_exact_at(primary_directory + table * 4, &mut entry)?;
        let p = u64::from(u32::from_le_bytes(entry)) * 512;
        let r = if redundant_directory != 0 {
            budget.work(1)?;
            source.read_exact_at(redundant_directory + table * 4, &mut entry)?;
            u64::from(u32::from_le_bytes(entry)) * 512
        } else {
            0
        };
        for within in 0..(count - table * 512).min(512) {
            primary.push((p != 0).then_some(p + within * 4));
            redundant.push((r != 0).then_some(r + within * 4));
        }
    }
    Ok(Slots {
        primary,
        redundant,
        redundant_required: redundant_directory != 0,
        primary_directory,
        redundant_directory,
        overhead,
        protected,
    })
}
pub(super) fn open_chain(path: &Path, authorized: &[PathBuf]) -> io::Result<Opened> {
    open_inner(path, authorized, true)
}
#[cfg(test)]
fn open(path: &Path, authorized: &[PathBuf]) -> io::Result<Opened> {
    open_inner(path, authorized, false)
}
fn open_inner(path: &Path, authorized: &[PathBuf], allow_parent: bool) -> io::Result<Opened> {
    open_policy(
        path,
        authorized,
        allow_parent,
        crate::RecoveryPolicy::Recover,
    )
}
pub(super) fn open_policy(
    path: &Path,
    authorized: &[PathBuf],
    allow_parent: bool,
    policy: crate::RecoveryPolicy,
) -> io::Result<Opened> {
    if authorized.len() > 256 {
        return Err(unsupported());
    }
    let path = path.canonicalize()?;
    let raw = Arc::new(RawWriter::open(&path)?);
    raw.require_single_link_for_journal()?;
    let identity = raw.opened_identity()?;
    let budget = ReadBudget::new(ParserLimits::default())?;
    if raw.is_empty() || raw.len() > 65536 {
        return Err(unsupported());
    }
    budget.metadata(raw.len())?;
    budget.work(1)?;
    // CID is the only descriptor patch in this profile; extent names and sizes stay intact during replay.
    let source: Arc<dyn ReadAt> = Arc::new(Source {
        raw: raw.clone(),
        size: raw.len(),
    });
    let desc = if allow_parent {
        descriptor_mode(&*source, true)?
    } else {
        descriptor(&*source)?
    };
    let mut authorization_cache = Vec::new();
    let mut approved = std::collections::BTreeSet::new();
    for authorized_path in authorized {
        budget.work(1)?;
        let canonical = authorized_path.canonicalize()?;
        let retained = canonical.as_os_str().as_encoded_bytes().len() as u64 + 128;
        budget.metadata(retained)?;
        authorization_cache.push(budget.cache(retained)?);
        approved.insert(canonical);
    }
    let mut files = vec![File {
        path: path.clone(),
        raw: raw.clone(),
        budget: budget.clone(),
    }];
    let mut identities = vec![identity];
    let mut physical = raw.len();
    for spec in &desc.specs {
        let extent = path
            .parent()
            .ok_or_else(invalid)?
            .join(&spec.name)
            .canonicalize()?;
        if !approved.contains(&extent) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "split sparse extent requires explicit authorization",
            ));
        }
        let writer = Arc::new(RawWriter::open(&extent)?);
        writer.require_single_link_for_journal()?;
        let id = writer.opened_identity()?;
        if identities.contains(&id) {
            return Err(unsupported());
        }
        identities.push(id);
        physical = physical.checked_add(writer.len()).ok_or_else(invalid)?;
        if physical > 33 * 1024 * 1024 * 1024 {
            return Err(unsupported());
        }
        files.push(File {
            path: extent,
            raw: writer,
            budget: budget.clone(),
        });
    }
    let parent = if allow_parent {
        Vmdk::resolve_pinned_parent(
            source,
            &path,
            authorized,
            &budget,
            &identities,
            files.len(),
            physical,
        )?
    } else {
        None
    };
    let validator = |sources: &[Arc<dyn ReadAt>]| {
        validate_sources_parented(sources, &budget, &files, parent.as_ref())
    };
    for file in &files {
        policy.check(crate::transaction::pending(&file.path)?)?;
    }
    if policy == crate::RecoveryPolicy::Recover {
        if parent.is_some() {
            transaction_set::recover_bound(&files, parent.as_ref(), &validator)?;
        } else {
            transaction_set::recover(&files, &validator)?;
        }
    }
    let sources = files
        .iter()
        .map(|file| {
            Arc::new(Source {
                raw: file.raw.clone(),
                size: file.raw.len(),
            }) as Arc<dyn ReadAt>
        })
        .collect::<Vec<_>>();
    let before = budget.usage();
    validate_sources_parented(&sources, &budget, &files, parent.as_ref())?;
    let after = budget.usage();
    let validation_work = after.work_items - before.work_items;
    let validation_metadata = after.metadata_bytes - before.metadata_bytes;
    let retained = desc
        .specs
        .iter()
        .map(|spec| spec.size.div_ceil(65536) * 64 + 256)
        .sum::<u64>()
        + files
            .iter()
            .map(|file| file.path.as_os_str().as_encoded_bytes().len() as u64 + 128)
            .sum::<u64>();
    let cache = budget.cache(retained)?;
    let mut extents = Vec::new();
    let mut all_mappings = Vec::new();
    let mut all_zero_mask = Vec::new();
    let mut all_overhead = Vec::new();
    let mut all_primary = Vec::new();
    let mut all_redundant = Vec::new();
    let mut start = 0;
    for (spec, source) in desc.specs.iter().zip(&sources[1..]) {
        let disk = Vmdk::open_with_budget(source.clone(), &budget)?;
        let mappings = disk.writer_mappings()?;
        let zero_mask = disk.writer_zero_mask();
        let Slots {
            primary,
            redundant,
            redundant_required,
            primary_directory,
            redundant_directory,
            overhead,
            protected,
        } = mapping_slots(&**source, spec.size, &budget)?;
        extents.push(Extent {
            start,
            size: spec.size,
            redundant_required,
            primary_directory,
            redundant_directory,
            protected,
        });
        all_overhead.push(overhead);
        all_primary.push(primary);
        all_redundant.push(redundant);
        all_mappings.push(mappings);
        all_zero_mask.push(zero_mask);
        start += spec.size;
    }
    Ok(Opened {
        raw: raw.clone(),
        identity: raw.opened_identity()?,
        path,
        sparse: Sparse {
            parent,
            files,
            extents,
            size: desc.size,
            cid_offset: desc.cid_offset,
            cid_width: desc.cid_width,
            state: Mutex::new(State {
                epoch: false,
                failed: false,
                mappings: all_mappings,
                zero_mask: all_zero_mask,
                primary: all_primary,
                redundant: all_redundant,
                overhead: all_overhead,
                validation_work,
                validation_metadata,
            }),
            _cache: cache,
            budget,
        },
    })
}
impl Sparse {
    pub(super) fn parent_reader(&self) -> Option<Arc<Vmdk>> {
        self.parent.as_ref().map(|graph| graph.reader.clone())
    }

    pub(super) fn container_set_size(&self) -> Option<u64> {
        self.files
            .iter()
            .try_fold(0u64, |sum, file| sum.checked_add(file.raw.len()))
    }

    fn commit(&self, records: Vec<Option<Record>>, cut: Option<usize>) -> io::Result<()> {
        let validator = |sources: &[Arc<dyn ReadAt>]| {
            validate_sources_parented(sources, &self.budget, &self.files, self.parent.as_ref())
        };
        if self.parent.is_some() {
            transaction_set::commit_bound(
                &self.files,
                records,
                self.parent.as_ref(),
                cut,
                &validator,
            )
        } else {
            transaction_set::commit(&self.files, records, cut, &validator)
        }
    }
    fn preflight(&self, offset: u64, length: u64, state: &State) -> io::Result<RequestPlan> {
        let count = self
            .extents
            .iter()
            .map(|extent| extent.size.div_ceil(65536).div_ceil(512))
            .sum::<u64>();
        let cache = self
            .budget
            .cache(count * 128 + self.extents.len() as u64 * 128)
            .map_err(|_| unsupported())?;
        let mut planned = vec![Vec::new(); self.extents.len()];
        let mut arenas = vec![None; self.extents.len()];
        let mut first_table = vec![None; self.extents.len()];
        let mut projected = self
            .files
            .iter()
            .map(|file| file.raw.len())
            .collect::<Vec<_>>();
        if let Some(parent) = &self.parent {
            parent.verify()?;
        }
        let _inherited_cache = self
            .parent
            .as_ref()
            .map(|_| self.budget.cache(65536))
            .transpose()?;
        let mut inherited = self.parent.as_ref().map(|_| vec![0; 65536]);
        let mut done = 0;
        let mut grains = 0u64;
        let mut new_tables = 0u64;
        let mut preparatory = 0u64;
        while done < length {
            let (index, extent, local) = self.locate(offset + done);
            let grain = (local / 65536) as usize;
            let table = grain / 512;
            first_table[index].get_or_insert(table);
            if state.mappings[index][grain] == 0 {
                // Read every inherited allocation before preparatory table/CID
                // publication, so parent read and cumulative budget failures
                // cannot first surface after an earlier extent was changed.
                if !state.zero_mask[index][grain]
                    && let Some(parent) = &self.parent
                {
                    let start = grain as u64 * 65536;
                    let count = (extent.size - start).min(65536) as usize;
                    parent.reader.read_exact_at(
                        extent.start + start,
                        &mut inherited.as_mut().ok_or_else(invalid)?[..count],
                    )?;
                }
                if !projected[index + 1].is_multiple_of(65536) {
                    return Err(unsupported());
                }
                if state.primary[index][grain].is_none()
                    || (extent.redundant_required && state.redundant[index][grain].is_none())
                {
                    if state.primary[index][grain].is_some()
                        || (extent.redundant_required && state.redundant[index][grain].is_some())
                    {
                        return Err(unsupported());
                    }
                    if !planned[index]
                        .iter()
                        .any(|plan: &TablePlan| plan.table == table)
                    {
                        planned[index].push(TablePlan {
                            table,
                            padding: None,
                        });
                    }
                }
                projected[index + 1] = projected[index + 1]
                    .checked_add(65536)
                    .ok_or_else(unsupported)?;
            }
            done += (65536 - local % 65536)
                .min(extent.size - local)
                .min(length - done);
            grains += 1;
        }
        for index in 0..self.extents.len() {
            if planned[index].is_empty() {
                continue;
            }
            planned[index].sort_unstable_by_key(|plan| plan.table);
            let bytes = 2048 * (1 + u64::from(self.extents[index].redundant_required));
            new_tables += planned[index].len() as u64
                * (1 + u64::from(self.extents[index].redundant_required));
            if state.mappings[index].iter().all(|mapping| *mapping == 0) {
                let length = (bytes * planned[index].len() as u64).div_ceil(65536) * 65536;
                let offset = self.files[index + 1].raw.len();
                let prepare = planned[index].len() != 1
                    || first_table[index] != Some(planned[index][0].table);
                arenas[index] = Some(ArenaPlan {
                    offset,
                    length,
                    prepare,
                });
                if prepare {
                    preparatory += 1;
                    for (position, plan) in planned[index].iter_mut().enumerate() {
                        plan.padding = Some(offset + position as u64 * bytes);
                    }
                }
                projected[index + 1] = projected[index + 1]
                    .checked_add(length)
                    .ok_or_else(unsupported)?;
            } else {
                for position in 0..planned[index].len() {
                    let offset = self
                        .padding(index, state, &planned[index][..position])
                        .map_err(|_| unsupported())?;
                    planned[index][position].padding = Some(offset);
                }
            }
            u32::try_from(projected[index + 1] / 512).map_err(|_| unsupported())?;
        }
        let dependency_bytes = self.parent.as_ref().map_or(Ok(0u64), |parent| {
            parent.dependencies.iter().try_fold(0u64, |sum, pin| {
                sum.checked_add(pin.length).ok_or_else(unsupported)
            })
        })?;
        let physical = projected.iter().try_fold(dependency_bytes, |sum, length| {
            sum.checked_add(*length).ok_or_else(unsupported)
        })?;
        if physical > 33 * 1024 * 1024 * 1024 {
            return Err(unsupported());
        }
        // Zeroing chunks can split one extent grain into two transactions.
        // Account for every future complete 512-entry table scan, even when
        // only one logical grain lives in a newly installed table.
        let scans = projected
            .iter()
            .map(|length| length.div_ceil(65536))
            .sum::<u64>()
            + self.parent.as_ref().map_or(0, |parent| {
                parent
                    .dependencies
                    .iter()
                    .map(|pin| pin.length.div_ceil(65536))
                    .sum::<u64>()
            });
        // A supported ancestor grain is at least one 512-byte sector.
        // Bound an allocating 64-KiB read through every retained ancestor,
        // including split dispatch and source reads, before a CID epoch.
        let inherited_work = self
            .parent
            .as_ref()
            .map_or(0, |parent| 128 * (parent.nodes.len() as u64 * 4 + 4));
        let possible_owners = self
            .extents
            .iter()
            .map(|extent| extent.size.div_ceil(65536))
            .sum::<u64>();
        let work = ((state.validation_work + new_tables * 513 + possible_owners * 2) * 4
            + scans * 5
            + inherited_work
            + 1032)
            .checked_mul(grains * 2 + preparatory)
            .ok_or_else(unsupported)?;
        let metadata = ((state.validation_metadata + new_tables * 2048) * 4 + 4 * 1024 * 1024)
            .checked_mul(grains * 2 + preparatory)
            .ok_or_else(unsupported)?;
        let usage = self.budget.usage();
        let limits = self.budget.limits();
        if usage
            .work_items
            .checked_add(work)
            .is_none_or(|value| value > limits.work_items)
            || usage
                .metadata_bytes
                .checked_add(metadata)
                .is_none_or(|value| value > limits.metadata_bytes)
            || usage
                .cache_bytes
                .checked_add(32 * 1024 * 1024)
                .is_none_or(|value| value > limits.cache_bytes)
        {
            return Err(unsupported());
        }
        Ok(RequestPlan {
            tables: planned,
            arenas,
            _cache: cache,
        })
    }
    fn padding(&self, index: usize, state: &State, reserved: &[TablePlan]) -> io::Result<u64> {
        let extent = &self.extents[index];
        let interval_bytes = (extent.protected.len() as u64
            + state.primary[index].len().div_ceil(512) as u64 * 2
            + reserved.len() as u64)
            * 16;
        self.budget.metadata(interval_bytes)?;
        let _scratch = self.budget.cache(interval_bytes)?;
        let mut protected = Vec::with_capacity((interval_bytes / 16) as usize);
        protected.extend_from_slice(&extent.protected);
        for slots in [&state.primary[index], &state.redundant[index]] {
            for table in slots.chunks(512) {
                self.budget.work(1)?;
                if let Some(start) = table[0] {
                    protected.push((start, start + 2048));
                }
            }
        }
        let needed = 2048 * (1 + u64::from(extent.redundant_required));
        for plan in reserved {
            let offset = plan.padding.ok_or_else(unsupported)?;
            protected.push((offset, offset + needed));
        }
        protected.sort_unstable();
        let mut candidate = 512;
        for (start, end) in protected {
            self.budget.work(1)?;
            if candidate + needed <= start && candidate + needed <= state.overhead[index] {
                return Ok(candidate);
            }
            candidate = candidate.max(end.div_ceil(512) * 512);
        }
        if candidate + needed <= state.overhead[index] {
            Ok(candidate)
        } else {
            Err(unsupported())
        }
    }
    fn epoch_record(&self, state: &State, records: &mut [Option<Record>]) -> io::Result<()> {
        if !state.epoch {
            let descriptor = &self.files[0].raw;
            let mut old = vec![0; self.cid_width];
            descriptor.read_exact_at(self.cid_offset, &mut old)?;
            let current =
                u32::from_str_radix(std::str::from_utf8(&old).map_err(|_| invalid())?, 16)
                    .map_err(|_| invalid())?;
            let mask = if self.cid_width == 8 {
                u32::MAX
            } else {
                (1u32 << (4 * self.cid_width)) - 1
            };
            let mut fresh = None;
            for _ in 0..4 {
                let mut bytes = [0; 4];
                getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
                let value = u32::from_le_bytes(bytes) & mask;
                if value != current && value != u32::MAX {
                    fresh = Some(format!("{value:0width$x}", width = self.cid_width).into_bytes());
                    break;
                }
            }
            let new =
                fresh.ok_or_else(|| io::Error::other("could not generate fresh split VMDK CID"))?;
            let source = Source {
                raw: descriptor.clone(),
                size: descriptor.len(),
            };
            self.budget.work(source.len().div_ceil(65536))?;
            records[0] = Some(Record {
                original_length: source.len(),
                final_length: source.len(),
                original_digest: transaction::digest_reader(&source)?,
                patches: vec![Patch {
                    order: 0,
                    offset: self.cid_offset,
                    old,
                    new,
                }],
            });
        }
        Ok(())
    }
    fn prepare(&self, plan: &RequestPlan, state: &mut State, cut: Option<usize>) -> io::Result<()> {
        for (index, arena) in plan.arenas.iter().enumerate() {
            let Some(arena) = arena.filter(|arena| arena.prepare) else {
                continue;
            };
            let file = &self.files[index + 1];
            let source = Source {
                raw: file.raw.clone(),
                size: file.raw.len(),
            };
            let extent = &self.extents[index];
            let tables = &plan.tables[index];
            let mut patches = vec![Patch {
                order: 0,
                offset: arena.offset,
                old: vec![],
                new: vec![0; arena.length as usize],
            }];
            let first = tables.first().ok_or_else(invalid)?.table;
            let last = tables.last().unwrap().table;
            for (directory, redundant) in [
                (extent.primary_directory, false),
                (extent.redundant_directory, true),
            ] {
                if directory == 0 {
                    continue;
                }
                let offset = directory + first as u64 * 4;
                let mut old = vec![0; (last - first + 1) * 4];
                file.raw.read_exact_at(offset, &mut old)?;
                let mut new = old.clone();
                for table in tables {
                    let physical =
                        table.padding.ok_or_else(invalid)? + if redundant { 2048 } else { 0 };
                    let entry = u32::try_from(physical / 512).map_err(|_| unsupported())?;
                    new[(table.table - first) * 4..(table.table - first + 1) * 4]
                        .copy_from_slice(&entry.to_le_bytes());
                }
                patches.push(Patch {
                    order: patches.len() as u32,
                    offset,
                    old,
                    new,
                });
            }
            let mut old = vec![0; 8];
            file.raw.read_exact_at(64, &mut old)?;
            patches.push(Patch {
                order: patches.len() as u32,
                offset: 64,
                old,
                new: ((arena.offset + arena.length) / 512).to_le_bytes().to_vec(),
            });
            patches.sort_unstable_by_key(|patch| patch.offset);
            self.budget.work(source.len().div_ceil(65536))?;
            let mut records = vec![None; self.files.len()];
            records[index + 1] = Some(Record {
                original_length: source.len(),
                final_length: arena.offset + arena.length,
                original_digest: transaction::digest_reader(&source)?,
                patches,
            });
            self.epoch_record(state, &mut records)?;
            self.commit(records, cut)?;
            state.epoch = true;
            state.overhead[index] = arena.offset + arena.length;
            for table in tables {
                let offset = table.padding.ok_or_else(invalid)?;
                let start = table.table * 512;
                for slot in start..(start + 512).min(state.primary[index].len()) {
                    state.primary[index][slot] = Some(offset + (slot - start) as u64 * 4);
                    if extent.redundant_required {
                        state.redundant[index][slot] =
                            Some(offset + 2048 + (slot - start) as u64 * 4);
                    }
                }
            }
            let count = tables.len() as u64 * (1 + u64::from(extent.redundant_required));
            state.validation_work += count * 513;
            state.validation_metadata += count * 2048;
        }
        Ok(())
    }
    pub(super) fn len(&self) -> u64 {
        self.size
    }
    fn state(&self) -> io::Result<std::sync::MutexGuard<'_, State>> {
        let state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("split sparse writer mutex poisoned"))?;
        if state.failed {
            return Err(io::Error::other(
                "split sparse transaction failed; reopen for recovery",
            ));
        }
        Ok(state)
    }
    fn locate(&self, offset: u64) -> (usize, &Extent, u64) {
        let index = self
            .extents
            .partition_point(|extent| extent.start <= offset)
            - 1;
        (
            index,
            &self.extents[index],
            offset - self.extents[index].start,
        )
    }
    pub(super) fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, bytes.len() as u64, self.size)?;
        let state = self.state()?;
        let mut done = 0;
        while done < bytes.len() {
            let (index, extent, local) = self.locate(offset + done as u64);
            let grain = (local / 65536) as usize;
            let take = (65536 - local % 65536)
                .min(extent.size - local)
                .min((bytes.len() - done) as u64) as usize;
            if state.mappings[index][grain] == 0 {
                if !state.zero_mask[index][grain]
                    && let Some(parent) = &self.parent
                {
                    parent
                        .reader
                        .read_exact_at(offset + done as u64, &mut bytes[done..done + take])?;
                } else {
                    bytes[done..done + take].fill(0);
                }
            } else {
                self.files[index + 1].raw.read_exact_at(
                    state.mappings[index][grain] + local % 65536,
                    &mut bytes[done..done + take],
                )?;
            }
            done += take;
        }
        Ok(())
    }
    pub(super) fn write_all_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.write_with_cut(offset, bytes, None)
    }
    fn write_with_cut(&self, offset: u64, bytes: &[u8], cut: Option<usize>) -> io::Result<()> {
        self.write_with_phase_cuts(offset, bytes, None, cut)
    }
    fn write_with_phase_cuts(
        &self,
        offset: u64,
        bytes: &[u8],
        prep_cut: Option<usize>,
        cut: Option<usize>,
    ) -> io::Result<()> {
        crate::check_range(offset, bytes.len() as u64, self.size)?;
        let mut state = self.state()?;
        let planned = self.preflight(offset, bytes.len() as u64, &state)?;
        let outcome = self
            .prepare(&planned, &mut state, prep_cut)
            .and_then(|()| self.write_locked(offset, bytes, cut, &mut state, &planned));
        if outcome.is_err() {
            state.failed = true;
        }
        outcome
    }
    fn write_locked(
        &self,
        offset: u64,
        bytes: &[u8],
        cut: Option<usize>,
        state: &mut State,
        planned: &RequestPlan,
    ) -> io::Result<()> {
        let mut done = 0;
        while done < bytes.len() {
            let (index, extent, local) = self.locate(offset + done as u64);
            let take = (65536 - local % 65536)
                .min(extent.size - local)
                .min((bytes.len() - done) as u64) as usize;
            let file = &self.files[index + 1];
            let source = Source {
                raw: file.raw.clone(),
                size: file.raw.len(),
            };
            let grain = (local / 65536) as usize;
            let allocating = state.mappings[index][grain] == 0;
            let creating_table = allocating && state.primary[index][grain].is_none();
            let table_plan = planned.tables[index]
                .iter()
                .find(|plan| plan.table == grain / 512);
            let padding = table_plan.and_then(|plan| plan.padding);
            let table_offset = padding.unwrap_or(source.len());
            let allocation_offset = source.len()
                + if creating_table && padding.is_none() {
                    65536
                } else {
                    0
                };
            let mut patches = Vec::new();
            if allocating {
                let entry = u32::try_from(allocation_offset / 512)
                    .map_err(|_| unsupported())?
                    .to_le_bytes()
                    .to_vec();
                let mut payload = vec![0; 65536];
                if !state.zero_mask[index][grain]
                    && let Some(parent) = &self.parent
                {
                    let start = grain as u64 * 65536;
                    let count = (extent.size - start).min(65536) as usize;
                    parent
                        .reader
                        .read_exact_at(extent.start + start, &mut payload[..count])?;
                }
                let within = (local % 65536) as usize;
                payload[within..within + take].copy_from_slice(&bytes[done..done + take]);
                patches.push(Patch {
                    order: u32::from(creating_table),
                    offset: allocation_offset,
                    old: vec![],
                    new: payload,
                });
                if !creating_table {
                    if let Some(offset) = state.redundant[index][grain] {
                        let mut old = vec![0; 4];
                        file.raw.read_exact_at(offset, &mut old)?;
                        patches.push(Patch {
                            order: 1,
                            offset,
                            old,
                            new: entry.clone(),
                        });
                    }
                    let offset = state.primary[index][grain].ok_or_else(unsupported)?;
                    let mut old = vec![0; 4];
                    file.raw.read_exact_at(offset, &mut old)?;
                    patches.push(Patch {
                        order: if state.redundant[index][grain].is_some() {
                            2
                        } else {
                            1
                        },
                        offset,
                        old,
                        new: entry,
                    });
                } else {
                    if table_plan.is_none() {
                        return Err(unsupported());
                    }
                    let arena_length = if padding.is_some() {
                        2048 * (1 + usize::from(extent.redundant_required))
                    } else {
                        65536
                    };
                    let mut arena = vec![0; arena_length];
                    let within = grain % 512 * 4;
                    arena[within..within + 4].copy_from_slice(&entry);
                    if extent.redundant_required {
                        arena[2048 + within..2052 + within].copy_from_slice(&entry);
                    }
                    let mut old = if padding.is_some() {
                        vec![0; arena_length]
                    } else {
                        vec![]
                    };
                    if padding.is_some() {
                        file.raw.read_exact_at(table_offset, &mut old)?;
                    }
                    patches.push(Patch {
                        order: 0,
                        offset: table_offset,
                        old,
                        new: arena,
                    });
                    for (directory, table_offset, order) in [
                        (
                            extent.primary_directory,
                            table_offset,
                            2 + u32::from(extent.redundant_required),
                        ),
                        (extent.redundant_directory, table_offset + 2048, 2),
                    ] {
                        if directory == 0 {
                            continue;
                        }
                        let offset = directory + (grain / 512) as u64 * 4;
                        let mut old = vec![0; 4];
                        file.raw.read_exact_at(offset, &mut old)?;
                        patches.push(Patch {
                            order,
                            offset,
                            old,
                            new: u32::try_from(table_offset / 512)
                                .map_err(|_| unsupported())?
                                .to_le_bytes()
                                .to_vec(),
                        });
                    }
                    if padding.is_none() {
                        let mut old = vec![0; 8];
                        file.raw.read_exact_at(64, &mut old)?;
                        patches.push(Patch {
                            order: 3 + u32::from(extent.redundant_required),
                            offset: 64,
                            old,
                            new: (allocation_offset / 512).to_le_bytes().to_vec(),
                        });
                    }
                }
                patches.sort_unstable_by_key(|patch| patch.offset);
            } else {
                let physical = state.mappings[index][grain] + local % 65536;
                let mut old = vec![0; take];
                file.raw.read_exact_at(physical, &mut old)?;
                patches.push(Patch {
                    order: 0,
                    offset: physical,
                    old,
                    new: bytes[done..done + take].to_vec(),
                });
            }
            self.budget.work(source.len().div_ceil(65536))?;
            let mut records = vec![None; self.files.len()];
            records[index + 1] = Some(Record {
                original_length: source.len(),
                final_length: if allocating {
                    allocation_offset + 65536
                } else {
                    source.len()
                },
                original_digest: transaction::digest_reader(&source)?,
                patches,
            });
            self.epoch_record(state, &mut records)?;
            if let Err(error) = self.commit(records, cut) {
                state.failed = true;
                return Err(error);
            }
            state.epoch = true;
            if creating_table {
                if padding.is_none() {
                    state.overhead[index] = allocation_offset;
                }
                let start = grain / 512 * 512;
                for slot in start..(start + 512).min(state.primary[index].len()) {
                    state.primary[index][slot] = Some(table_offset + (slot - start) as u64 * 4);
                    if extent.redundant_required {
                        state.redundant[index][slot] =
                            Some(table_offset + 2048 + (slot - start) as u64 * 4);
                    }
                }
                let tables = 1 + u64::from(extent.redundant_required);
                state.validation_work += tables * 513;
                state.validation_metadata += tables * 2048;
            }
            if allocating {
                state.mappings[index][grain] = allocation_offset;
                state.zero_mask[index][grain] = false;
            }
            done += take;
        }
        Ok(())
    }
    pub(super) fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        crate::check_range(offset, length, self.size)?;
        let mut state = self.state()?;
        let planned = self.preflight(offset, length, &state)?;
        if let Err(error) = self.prepare(&planned, &mut state, None) {
            state.failed = true;
            return Err(error);
        }
        let mut done = 0;
        let zeros = [0; 65536];
        while done < length {
            let take = (length - done).min(65536) as usize;
            if let Err(error) =
                self.write_locked(offset + done, &zeros[..take], None, &mut state, &planned)
            {
                state.failed = true;
                return Err(error);
            }
            done += take as u64;
        }
        Ok(())
    }
    pub(super) fn flush(&self) -> io::Result<()> {
        let mut state = self.state()?;
        for file in &self.files {
            file.raw.flush()?;
        }
        state.epoch = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("disk.vmdk");
        let mut extents = Vec::new();
        for index in 1..=2 {
            let extent = directory.path().join(format!("disk-s{index}.vmdk"));
            let writer = crate::VmdkWriter::create(&extent, 65536).unwrap();
            writer.write_all_at(0, &vec![index as u8; 65536]).unwrap();
            writer.flush().unwrap();
            drop(writer);
            let mut bytes = std::fs::read(&extent).unwrap();
            let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
            let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
            bytes[offset..offset + length].fill(0);
            std::fs::write(&extent, bytes).unwrap();
            extents.push(extent);
        }
        std::fs::write(&path,"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"disk-s1.vmdk\"\nRW 128 SPARSE \"disk-s2.vmdk\"\n").unwrap();
        (directory, path, extents)
    }
    fn backed_fixture(
        missing_tables: bool,
        redundant_tables: bool,
    ) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>, Vec<PathBuf>) {
        let (directory, parent, parent_extents) = fixture();
        let child = directory.path().join("child.vmdk");
        let mut extents = Vec::new();
        for (index, parent_extent) in parent_extents.iter().enumerate() {
            let extent = directory.path().join(format!("child-s{}.vmdk", index + 1));
            std::fs::copy(parent_extent, &extent).unwrap();
            extents.push(extent);
        }
        holes(&extents);
        if redundant_tables {
            redundant(&extents);
        }
        if missing_tables {
            for extent in &extents {
                let mut bytes = std::fs::read(extent).unwrap();
                for field in [48, 56] {
                    let directory = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap())
                        as usize
                        * 512;
                    if directory != 0 {
                        bytes[directory..directory + 4].fill(0);
                    }
                }
                std::fs::write(extent, bytes).unwrap();
            }
        }
        std::fs::write(&child,"version=1\nCID=87654321\nparentCID=12345678\nparentFileNameHint=\"disk.vmdk\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"child-s1.vmdk\"\nRW 128 SPARSE \"child-s2.vmdk\"\n").unwrap();
        let mut parents = vec![parent];
        parents.extend(parent_extents);
        (directory, child, extents, parents)
    }
    fn directory_snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        files.sort();
        files
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect()
    }
    fn mapped_first_grain(path: &Path) -> bool {
        let bytes = std::fs::read(path).unwrap();
        let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
        let gt = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
        gt != 0 && bytes[gt..gt + 4] != [0; 4]
    }
    #[test]
    fn backed_cow_recovery_cuts_bind_parent_set_and_preserve_cid_barrier() {
        let _boundary = crate::test_sync::writer_test();
        for missing_tables in [false, true] {
            for redundant_tables in [false, true] {
                let mut interrupted = 0;
                for cut in 0..24 {
                    let (_directory, path, extents, parents) =
                        backed_fixture(missing_tables, redundant_tables);
                    let parent_before = parents
                        .iter()
                        .map(|path| std::fs::read(path).unwrap())
                        .collect::<Vec<_>>();
                    let descriptor_before = std::fs::read(&path).unwrap();
                    let mut authorized = extents.clone();
                    authorized.extend(parents.clone());
                    let opened = open_chain(&path, &authorized).unwrap();
                    let outcome = opened.sparse.write_with_cut(17, &[7; 19], Some(cut));
                    if let Err(error) = outcome {
                        assert_eq!(
                            error.kind(),
                            io::ErrorKind::Interrupted,
                            "missing={missing_tables}, redundant={redundant_tables}, cut={cut}"
                        );
                        interrupted += 1;
                        assert!(opened.sparse.read_exact_at(0, &mut [0; 1]).is_err());
                    }
                    for parent in &parents {
                        assert!(
                            !transaction::pending(parent).unwrap(),
                            "parents never have participant markers"
                        );
                    }
                    if std::fs::read(&path).unwrap() == descriptor_before {
                        assert!(
                            !mapped_first_grain(&extents[0]),
                            "CID must publish before private mapping"
                        );
                    }
                    drop(opened);
                    let reopened = crate::VmdkWriter::open_chain(&path, &authorized).unwrap();
                    let mut actual = vec![0; 131072];
                    reopened.read_exact_at(0, &mut actual).unwrap();
                    let mut expected = vec![1; 65536];
                    expected.extend(vec![2; 65536]);
                    expected[17..36].fill(7);
                    assert_eq!(
                        actual, expected,
                        "missing={missing_tables}, redundant={redundant_tables}, cut={cut}"
                    );
                    assert!(!transaction::pending(&path).unwrap());
                    for extent in &extents {
                        assert!(!transaction::pending(extent).unwrap());
                    }
                    for (parent, original) in parents.iter().zip(&parent_before) {
                        assert_eq!(std::fs::read(parent).unwrap(), *original);
                    }
                }
                assert!(
                    interrupted >= 12,
                    "must cover complete publication, replay and cleanup barriers"
                );
            }
        }
    }
    #[test]
    fn backed_recovery_refuses_missing_or_wrong_parent_authority_without_cleanup() {
        let _boundary = crate::test_sync::writer_test();
        for cut in [0, 5] {
            let (directory, path, extents, parents) = backed_fixture(false, true);
            let mut authorized = extents.clone();
            authorized.extend(parents.clone());
            let opened = open_chain(&path, &authorized).unwrap();
            assert_eq!(
                opened
                    .sparse
                    .write_with_cut(17, &[7; 19], Some(cut))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Interrupted
            );
            drop(opened);
            let wrong = directory.path().join("wrong-parent.vmdk");
            std::fs::copy(&parents[0], &wrong).unwrap();
            let before = directory_snapshot(directory.path());
            let mut denied = extents.clone();
            denied.extend(parents[1..].iter().cloned());
            assert!(crate::VmdkWriter::open_chain(&path, &denied).is_err());
            assert_eq!(directory_snapshot(directory.path()), before);
            denied.push(wrong);
            assert!(crate::VmdkWriter::open_chain(&path, &denied).is_err());
            assert_eq!(directory_snapshot(directory.path()), before);
            drop(crate::VmdkWriter::open_chain(&path, &authorized).unwrap());
        }
    }
    #[test]
    fn backed_recovery_rejects_changed_parent_bytes_lengths_and_identities_before_mutation() {
        let _boundary = crate::test_sync::writer_test();
        for change in 0..5 {
            for cut in [0, 5] {
                let (directory, path, extents, parents) = backed_fixture(false, false);
                let mut authorized = extents.clone();
                authorized.extend(parents.clone());
                let opened = open_chain(&path, &authorized).unwrap();
                assert_eq!(
                    opened
                        .sparse
                        .write_with_cut(17, &[7; 19], Some(cut))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
                drop(opened);
                match change {
                    0 => {
                        let mut bytes = std::fs::read(&parents[0]).unwrap();
                        bytes.extend_from_slice(b"# same CID but changed descriptor\n");
                        std::fs::write(&parents[0], bytes).unwrap();
                    }
                    1 => {
                        let mut bytes = std::fs::read(&parents[2]).unwrap();
                        *bytes.last_mut().unwrap() ^= 1;
                        std::fs::write(&parents[2], bytes).unwrap();
                    }
                    2 => {
                        let file = std::fs::OpenOptions::new()
                            .write(true)
                            .open(&parents[2])
                            .unwrap();
                        file.set_len(file.metadata().unwrap().len() + 65536)
                            .unwrap();
                    }
                    3 => {
                        let bytes = std::fs::read(&parents[2]).unwrap();
                        std::fs::rename(&parents[2], parents[2].with_extension("old")).unwrap();
                        std::fs::write(&parents[2], bytes).unwrap();
                    }
                    _ => {
                        std::fs::write(
                            transaction::sidecar(&parents[2]),
                            b"foreign-parent-journal",
                        )
                        .unwrap();
                    }
                }
                let before = directory_snapshot(directory.path());
                assert!(
                    crate::VmdkWriter::open_chain(&path, &authorized).is_err(),
                    "change={change}, cut={cut}"
                );
                assert_eq!(
                    directory_snapshot(directory.path()),
                    before,
                    "rejection must preserve every image and journal"
                );
            }
        }
    }
    #[test]
    fn backed_child_refuses_unbound_v1_recovery_without_cleanup() {
        let _boundary = crate::test_sync::writer_test();
        let (directory, path, extents, parents) = backed_fixture(false, false);
        let mut authorized = extents;
        authorized.extend(parents);
        let opened = open_chain(&path, &authorized).unwrap();
        let mut records = vec![None; opened.sparse.files.len()];
        opened
            .sparse
            .epoch_record(&opened.sparse.state().unwrap(), &mut records)
            .unwrap();
        assert_eq!(
            transaction_set::commit(&opened.sparse.files, records, Some(0), &|sources| {
                validate_sources_parented(
                    sources,
                    &opened.sparse.budget,
                    &opened.sparse.files,
                    opened.sparse.parent.as_ref(),
                )
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(
            std::fs::read(transaction::sidecar(&path))
                .unwrap()
                .starts_with(b"VDTXSET1")
        );
        drop(opened);
        let before = directory_snapshot(directory.path());
        assert!(crate::VmdkWriter::open_chain(&path, &authorized).is_err());
        assert_eq!(directory_snapshot(directory.path()), before);
    }
    #[test]
    #[ignore = "requires independent qemu-img backed split COW recovery oracle"]
    fn backed_cow_recovery_and_zero_masks_match_qemu_full_contents() {
        let _boundary = crate::test_sync::subprocess_test();
        for missing_tables in [false, true] {
            for redundant_tables in [false, true] {
                for zero_mask in [false, true] {
                    if zero_mask && missing_tables {
                        continue;
                    }
                    for cut in 0..24 {
                        let (directory, path, extents, parents) =
                            backed_fixture(missing_tables, redundant_tables);
                        if zero_mask {
                            let mut bytes = std::fs::read(&extents[0]).unwrap();
                            let flags = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) | 4;
                            bytes[8..12].copy_from_slice(&flags.to_le_bytes());
                            for field in [48, 56] {
                                let gd =
                                    u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap())
                                        as usize
                                        * 512;
                                if gd != 0 {
                                    let gt =
                                        u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap())
                                            as usize
                                            * 512;
                                    bytes[gt..gt + 4].copy_from_slice(&1u32.to_le_bytes());
                                }
                            }
                            std::fs::write(&extents[0], bytes).unwrap();
                        }
                        let parent_before = parents
                            .iter()
                            .map(|path| std::fs::read(path).unwrap())
                            .collect::<Vec<_>>();
                        let mut authorized = extents.clone();
                        authorized.extend(parents.clone());
                        let opened = open_chain(&path, &authorized).unwrap();
                        if let Err(error) = opened.sparse.write_with_cut(17, &[7; 19], Some(cut)) {
                            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                        }
                        drop(opened);
                        let reopened = crate::VmdkWriter::open_chain(&path, &authorized).unwrap();
                        reopened.write_zeroes(65536 + 19, 17).unwrap();
                        reopened.flush().unwrap();
                        drop(reopened);
                        let raw = directory.path().join("oracle.raw");
                        let result = std::process::Command::new("qemu-img")
                            .args(["convert", "-f", "vmdk", "-O", "raw"])
                            .arg(&path)
                            .arg(&raw)
                            .output()
                            .unwrap();
                        assert!(
                            result.status.success(),
                            "missing={missing_tables}, redundant={redundant_tables}, ZERO={zero_mask}, cut={cut}: {}",
                            String::from_utf8_lossy(&result.stderr)
                        );
                        let mut expected = vec![if zero_mask { 0 } else { 1 }; 65536];
                        expected.extend(vec![2; 65536]);
                        expected[17..36].fill(7);
                        expected[65536 + 19..65536 + 36].fill(0);
                        assert_eq!(
                            std::fs::read(raw).unwrap(),
                            expected,
                            "missing={missing_tables}, redundant={redundant_tables}, ZERO={zero_mask}, cut={cut}"
                        );
                        for (parent, original) in parents.iter().zip(&parent_before) {
                            assert_eq!(std::fs::read(parent).unwrap(), *original);
                            assert!(!transaction::pending(parent).unwrap());
                        }
                    }
                }
            }
        }
    }
    #[test]
    #[ignore = "requires qemu-img and qemu-io native split parent/child producer oracle"]
    fn qemu_produced_split_parent_child_import_preserves_cow_and_zero_masks() {
        let _boundary = crate::test_sync::subprocess_test();
        fn success(command: &mut std::process::Command) -> Vec<u8> {
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "native producer command {command:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            result.stdout
        }
        fn extents(directory: &Path, stem: &str) -> Vec<PathBuf> {
            let mut paths = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with(&format!("{stem}-s"))
                })
                .collect::<Vec<_>>();
            paths.sort();
            assert!(!paths.is_empty());
            paths
        }
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent.vmdk");
        let child = directory.path().join("child.vmdk");
        let version = success(std::process::Command::new("qemu-img").arg("--version"));
        eprintln!("{}", String::from_utf8_lossy(&version));
        success(
            std::process::Command::new("qemu-img")
                .args([
                    "create",
                    "-f",
                    "vmdk",
                    "-o",
                    "subformat=twoGbMaxExtentSparse",
                ])
                .arg(&parent)
                .arg("131072"),
        );
        success(
            std::process::Command::new("qemu-io")
                .args(["-f", "vmdk", "-c", "write -P 49 0 131072"])
                .arg(&parent),
        );
        success(
            std::process::Command::new("qemu-img")
                .args(["create", "-f", "vmdk", "-F", "vmdk", "-b"])
                .arg(&parent)
                .args(["-o", "subformat=twoGbMaxExtentSparse,zeroed_grain=on"])
                .arg(&child),
        );
        success(
            std::process::Command::new("qemu-io")
                .args(["-f", "vmdk", "-c", "write -z 0 65536"])
                .arg(&child),
        );
        let info = success(
            std::process::Command::new("qemu-img")
                .args(["info", "--backing-chain", "--output=json"])
                .arg(&child),
        );
        assert!(String::from_utf8(info).unwrap().contains("parent.vmdk"));
        let child_extents = extents(directory.path(), "child");
        let mut parents = vec![parent];
        parents.extend(extents(directory.path(), "parent"));
        let parent_before = parents
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        let mut authorized = child_extents.clone();
        authorized.extend(parents.clone());
        let mut expected = vec![0; 65536];
        expected.extend(vec![49; 65536]);
        let before = directory.path().join("before.raw");
        success(
            std::process::Command::new("qemu-img")
                .args(["convert", "-f", "vmdk", "-O", "raw"])
                .arg(&child)
                .arg(&before),
        );
        assert_eq!(std::fs::read(before).unwrap(), expected);
        let opened = open_chain(&child, &authorized).unwrap();
        assert!(
            opened.sparse.state().unwrap().zero_mask[0][0],
            "QEMU must produce a native ZERO GTE"
        );
        let mut actual = vec![255; 131072];
        opened.sparse.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, expected);
        opened.sparse.write_all_at(17, &[7; 19]).unwrap();
        opened.sparse.write_zeroes(65536 + 19, 17).unwrap();
        opened.sparse.flush().unwrap();
        drop(opened);
        expected[17..36].fill(7);
        expected[65536 + 19..65536 + 36].fill(0);
        let after = directory.path().join("after.raw");
        success(
            std::process::Command::new("qemu-img")
                .args(["convert", "-f", "vmdk", "-O", "raw"])
                .arg(&child)
                .arg(&after),
        );
        assert_eq!(std::fs::read(after).unwrap(), expected);
        let writer = crate::VmdkWriter::open_chain(&child, &authorized).unwrap();
        writer.write_zeroes(0, 65536).unwrap();
        writer.flush().unwrap();
        drop(writer);
        expected[..65536].fill(0);
        let zeroed = directory.path().join("zeroed.raw");
        success(
            std::process::Command::new("qemu-img")
                .args(["convert", "-f", "vmdk", "-O", "raw"])
                .arg(&child)
                .arg(&zeroed),
        );
        assert_eq!(std::fs::read(zeroed).unwrap(), expected);
        for (parent, original) in parents.iter().zip(&parent_before) {
            assert_eq!(std::fs::read(parent).unwrap(), *original);
            assert!(!transaction::pending(parent).unwrap());
        }
    }
    fn holes(extents: &[PathBuf]) {
        for extent in extents {
            let mut bytes = std::fs::read(extent).unwrap();
            for at in [48, 56] {
                let gd = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize * 512;
                if gd != 0 {
                    let table =
                        u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
                    bytes[table..table + 4].fill(0);
                }
            }
            std::fs::write(extent, bytes).unwrap();
        }
    }
    fn redundant(extents: &[PathBuf]) {
        for extent in extents {
            let mut bytes = std::fs::read(extent).unwrap();
            let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
            let table = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
            let copy = bytes[table..table + 2048].to_vec();
            let flags = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) | 2;
            bytes[8..12].copy_from_slice(&flags.to_le_bytes());
            bytes[48..56].copy_from_slice(&30u64.to_le_bytes());
            bytes[30 * 512..30 * 512 + 4].copy_from_slice(&31u32.to_le_bytes());
            bytes[31 * 512..31 * 512 + 2048].copy_from_slice(&copy);
            std::fs::write(extent, bytes).unwrap();
        }
    }
    #[test]
    #[ignore = "requires independent qemu-img multiple-table allocation oracle"]
    fn multiple_table_preparation_and_padding_match_qemu_full_contents() {
        let _boundary = crate::test_sync::subprocess_test();
        for allocated in [false, true] {
            let (directory, path, extents) = fixture();
            let mut bytes = std::fs::read(&extents[0]).unwrap();
            let grains = if allocated { 1025u64 } else { 513 };
            bytes[12..20].copy_from_slice(&(grains * 128).to_le_bytes());
            let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
            let gt = u32::from_le_bytes(bytes[gd..gd + 4].try_into().unwrap()) as usize * 512;
            if !allocated {
                bytes[gt..gt + 2048].fill(0);
            }
            std::fs::write(&extents[0], bytes).unwrap();
            redundant(&extents);
            let mut bytes = std::fs::read(&extents[0]).unwrap();
            for field in [48, 56] {
                let gd =
                    u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap()) as usize * 512;
                bytes[gd + usize::from(allocated) * 4..gd + grains.div_ceil(512) as usize * 4]
                    .fill(0);
            }
            std::fs::write(&extents[0], bytes).unwrap();
            let descriptor = std::fs::read_to_string(&path).unwrap().replace(
                "RW 128 SPARSE \"disk-s1.vmdk\"",
                &format!("RW {} SPARSE \"disk-s1.vmdk\"", grains * 128),
            );
            std::fs::write(&path, descriptor).unwrap();
            let boundary = if allocated {
                1024 * 65536u64
            } else {
                512 * 65536
            };
            let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
            writer.write_all_at(boundary - 1, &[7; 2]).unwrap();
            writer.flush().unwrap();
            drop(writer);
            let output = directory.path().join("multiple.raw");
            let result = std::process::Command::new("qemu-img")
                .args(["convert", "-f", "vmdk", "-O", "raw"])
                .arg(&path)
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut expected = vec![0; (grains as usize + 1) * 65536];
            if allocated {
                expected[..65536].fill(1);
            }
            expected[grains as usize * 65536..].fill(2);
            expected[boundary as usize - 1..boundary as usize + 1].fill(7);
            assert_eq!(
                std::fs::read(output).unwrap(),
                expected,
                "allocated={allocated}"
            );
        }
    }
    #[test]
    fn prepared_arena_cuts_keep_logical_old_then_payload_prefix() {
        let _boundary = crate::test_sync::writer_test();
        for redundant_tables in [false, true] {
            for preparation in [false, true] {
                let mut interrupted = 0;
                for cut in 0..24 {
                    let (_directory, path, extents) = fixture();
                    holes(&extents);
                    if redundant_tables {
                        redundant(&extents);
                    }
                    let mut bytes = std::fs::read(&extents[0]).unwrap();
                    bytes[12..20].copy_from_slice(&65664u64.to_le_bytes());
                    for field in [48, 56] {
                        let gd = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap())
                            as usize
                            * 512;
                        if gd != 0 {
                            bytes[gd..gd + 8].fill(0);
                        }
                    }
                    let eof = bytes.len();
                    std::fs::write(&extents[0], bytes).unwrap();
                    let descriptor = std::fs::read_to_string(&path).unwrap().replace(
                        "RW 128 SPARSE \"disk-s1.vmdk\"",
                        "RW 65664 SPARSE \"disk-s1.vmdk\"",
                    );
                    std::fs::write(&path, descriptor).unwrap();
                    let opened = open(&path, &extents).unwrap();
                    let boundary = 512 * 65536;
                    let outcome = opened.sparse.write_with_phase_cuts(
                        boundary - 1,
                        &[7; 2],
                        if preparation { Some(cut) } else { None },
                        if preparation { None } else { Some(cut) },
                    );
                    let failed = outcome.is_err();
                    if let Err(error) = outcome {
                        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                        interrupted += 1;
                        assert!(opened.sparse.read_exact_at(0, &mut [0]).is_err());
                        let state = opened.sparse.state.lock().unwrap();
                        assert_eq!(state.primary[0][511].is_none(), preparation);
                        assert_eq!(state.mappings[0][511], 0);
                    }
                    drop(opened);
                    let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
                    let mut actual = [0; 4];
                    writer.read_exact_at(boundary - 2, &mut actual).unwrap();
                    let expected = if failed && preparation {
                        [0, 0, 0, 0]
                    } else if failed {
                        [0, 7, 0, 0]
                    } else {
                        [0, 7, 7, 0]
                    };
                    assert_eq!(
                        actual, expected,
                        "redundant={redundant_tables} prep={preparation} cut={cut}"
                    );
                    let after = std::fs::read(&extents[0]).unwrap();
                    assert_eq!(
                        u64::from_le_bytes(after[64..72].try_into().unwrap()) * 512,
                        (eof + 65536) as u64
                    );
                }
                assert_eq!(
                    interrupted,
                    if preparation { 15 } else { 11 } + usize::from(redundant_tables)
                );
            }
        }
    }
    #[test]
    fn two_grains_share_one_new_table_arena() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        holes(&extents);
        let mut bytes = std::fs::read(&extents[1]).unwrap();
        bytes[12..20].copy_from_slice(&256u64.to_le_bytes());
        let gd = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize * 512;
        bytes[gd..gd + 4].fill(0);
        let eof = bytes.len() as u64;
        std::fs::write(&extents[1], bytes).unwrap();
        let descriptor = std::fs::read_to_string(&path).unwrap().replace(
            "RW 128 SPARSE \"disk-s2.vmdk\"",
            "RW 256 SPARSE \"disk-s2.vmdk\"",
        );
        std::fs::write(&path, descriptor).unwrap();
        let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
        writer.write_all_at(65536, &vec![7; 65537]).unwrap();
        writer.write_zeroes(131070, 4).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let reader = Vmdk::open_descriptor(&path, &extents).unwrap();
        let mut actual = vec![0; 196608];
        reader.read_exact_at(0, &mut actual).unwrap();
        let mut expected = vec![0; 196608];
        expected[65536..131073].fill(7);
        expected[131070..131074].fill(0);
        assert_eq!(actual, expected);
        assert_eq!(
            std::fs::metadata(&extents[1]).unwrap().len(),
            eof + 3 * 65536
        );
    }
    #[test]
    fn padding_creation_cuts_preserve_mapped_prefix_and_nonzero_original_hole() {
        let _boundary = crate::test_sync::writer_test();
        for redundant_tables in [false, true] {
            for cut in 0..20 {
                let (_directory, path, extents) = fixture();
                if redundant_tables {
                    redundant(&extents);
                }
                let mut bytes = std::fs::read(&extents[0]).unwrap();
                bytes[12..20].copy_from_slice(&65664u64.to_le_bytes());
                for field in [48, 56] {
                    let gd = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap())
                        as usize
                        * 512;
                    if gd != 0 {
                        bytes[gd + 4..gd + 8].fill(0);
                    }
                }
                let hole = if redundant_tables { 35 * 512 } else { 26 * 512 };
                let hole_len = if redundant_tables { 4096 } else { 2048 };
                bytes[hole..hole + hole_len].fill(83);
                std::fs::write(&extents[0], &bytes).unwrap();
                let descriptor = std::fs::read_to_string(&path).unwrap().replace(
                    "RW 128 SPARSE \"disk-s1.vmdk\"",
                    "RW 65664 SPARSE \"disk-s1.vmdk\"",
                );
                std::fs::write(&path, descriptor).unwrap();
                let opened = open(&path, &extents).unwrap();
                let result = opened
                    .sparse
                    .write_with_cut(512 * 65536 + 17, &[7; 19], Some(cut));
                if let Err(error) = result {
                    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                    assert!(opened.sparse.state.lock().unwrap().primary[0][512].is_none());
                    if cut == 0 {
                        opened.sparse.files[1]
                            .raw
                            .write_all_at(hole as u64, &[0])
                            .unwrap();
                        opened.sparse.files[1].raw.flush().unwrap();
                    }
                }
                drop(opened);
                let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
                let mut old = [0; 65536];
                writer.read_exact_at(0, &mut old).unwrap();
                assert_eq!(old, [1; 65536]);
                let mut last = [0; 65536];
                writer.read_exact_at(512 * 65536, &mut last).unwrap();
                let mut expected = [0; 65536];
                expected[17..36].fill(7);
                assert_eq!(last, expected);
                let after = std::fs::read(&extents[0]).unwrap();
                assert_eq!(&after[64..72], &bytes[64..72]);
                assert_eq!(after.len(), bytes.len() + 65536);
            }
        }
    }
    #[test]
    fn missing_table_cuts_recover_metadata_arena_and_torn_directory() {
        let _boundary = crate::test_sync::writer_test();
        for redundant_tables in [false, true] {
            for cut in 0..20 {
                let (_directory, path, extents) = fixture();
                holes(&extents);
                if redundant_tables {
                    redundant(&extents);
                }
                let mut bytes = std::fs::read(&extents[0]).unwrap();
                for field in [48, 56] {
                    let gd = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap())
                        as usize
                        * 512;
                    if gd != 0 {
                        bytes[gd..gd + 4].fill(0);
                    }
                }
                std::fs::write(&extents[0], bytes).unwrap();
                let opened = open(&path, &extents).unwrap();
                let eof = opened.sparse.files[1].raw.len();
                let result = opened.sparse.write_with_cut(17, &[7; 19], Some(cut));
                if let Err(error) = result {
                    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                    assert!(opened.sparse.state.lock().unwrap().primary[0][0].is_none());
                    if cut == 0 {
                        let pointer = u32::try_from(eof / 512).unwrap().to_le_bytes();
                        opened.sparse.files[1]
                            .raw
                            .write_all_at(opened.sparse.extents[0].primary_directory, &pointer[..1])
                            .unwrap();
                        let overhead = ((eof + 65536) / 512).to_le_bytes();
                        opened.sparse.files[1]
                            .raw
                            .write_all_at(64, &overhead[..1])
                            .unwrap();
                        opened.sparse.files[1].raw.flush().unwrap();
                    }
                }
                drop(opened);
                let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
                let mut actual = vec![0; 131072];
                writer.read_exact_at(0, &mut actual).unwrap();
                let mut expected = vec![0; 131072];
                expected[17..36].fill(7);
                assert_eq!(actual, expected);
                assert_eq!(std::fs::metadata(&extents[0]).unwrap().len(), eof + 131072);
            }
        }
    }
    #[test]
    fn allocation_publication_cuts_preserve_zero_neighbors_and_defer_cached_mapping() {
        let _boundary = crate::test_sync::writer_test();
        let mut cuts = 0;
        for has_redundant in [false, true] {
            for cut in 0..20 {
                let (_directory, path, extents) = fixture();
                holes(&extents);
                if has_redundant {
                    redundant(&extents);
                }
                let opened = open(&path, &extents).unwrap();
                let eof = opened.sparse.files[1].raw.len();
                let outcome = opened.sparse.write_with_cut(17, &[7; 19], Some(cut));
                if let Err(error) = outcome {
                    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                    cuts += 1;
                    assert_eq!(opened.sparse.state.lock().unwrap().mappings[0][0], 0);
                    assert!(opened.sparse.read_exact_at(0, &mut [0]).is_err());
                } else {
                    assert_eq!(opened.sparse.state.lock().unwrap().mappings[0][0], eof);
                }
                drop(opened);
                if transaction::pending(&path).unwrap() {
                    assert!(Vmdk::open_descriptor(&path, &extents).is_err());
                }
                for extent in &extents {
                    if transaction::pending(extent).unwrap() {
                        assert!(crate::VmdkWriter::open(extent).is_err());
                    }
                }
                let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
                let mut actual = vec![0; 131072];
                writer.read_exact_at(0, &mut actual).unwrap();
                let mut expected = vec![0; 131072];
                expected[17..36].fill(7);
                assert_eq!(actual, expected);
                assert_eq!(std::fs::metadata(&extents[0]).unwrap().len(), eof + 65536);
            }
        }
        assert_eq!(cuts, 29);
    }
    #[test]
    fn torn_primary_gte_recovers_before_native_reader_can_see_mismatched_tables() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        holes(&extents);
        redundant(&extents);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&extents[0])
            .unwrap()
            .set_len(513 * 65536)
            .unwrap();
        let opened = open(&path, &extents).unwrap();
        let eof = opened.sparse.files[1].raw.len();
        let slot = opened.sparse.state.lock().unwrap().primary[0][0].unwrap();
        assert!(opened.sparse.write_with_cut(17, &[7; 19], Some(0)).is_err());
        let entry = u32::try_from(eof / 512).unwrap().to_le_bytes();
        assert_ne!(entry[0], 0);
        assert_ne!(entry[2], 0);
        opened.sparse.files[1]
            .raw
            .write_all_at(slot, &entry[..1])
            .unwrap();
        opened.sparse.files[1].raw.flush().unwrap();
        drop(opened);
        assert!(Vmdk::open_descriptor(&path, &extents).is_err());
        let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
        let mut actual = vec![0; 131072];
        writer.read_exact_at(0, &mut actual).unwrap();
        let mut expected = vec![0; 131072];
        expected[17..36].fill(7);
        assert_eq!(actual, expected);
    }
    #[test]
    #[ignore = "requires independent qemu-img split allocation recovery oracle"]
    fn allocation_recovery_cuts_match_qemu_at_every_boundary() {
        let _boundary = crate::test_sync::subprocess_test();
        for has_redundant in [false, true] {
            for cut in 0..=if has_redundant { 14 } else { 13 } {
                let (directory, path, extents) = fixture();
                holes(&extents);
                if has_redundant {
                    redundant(&extents);
                }
                let opened = open(&path, &extents).unwrap();
                assert_eq!(
                    opened
                        .sparse
                        .write_with_cut(17, &[7; 19], Some(cut))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Interrupted
                );
                drop(opened);
                drop(crate::VmdkWriter::open_descriptor(&path, &extents).unwrap());
                let output = directory.path().join("after.raw");
                let result = std::process::Command::new("qemu-img")
                    .args(["convert", "-f", "vmdk", "-O", "raw"])
                    .arg(&path)
                    .arg(&output)
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let mut expected = vec![0; 131072];
                expected[17..36].fill(7);
                assert_eq!(
                    std::fs::read(output).unwrap(),
                    expected,
                    "redundant={has_redundant}, cut={cut}"
                );
            }
        }
    }
    #[test]
    fn every_publication_cut_recovers_whole_set_and_pending_extents_refuse() {
        let _boundary = crate::test_sync::writer_test();
        let mut interrupted = 0;
        for cut in 0..24 {
            let (_directory, path, extents) = fixture();
            let opened = open(&path, &extents).unwrap();
            let outcome = opened.sparse.write_with_cut(17, &[7; 19], Some(cut));
            if let Err(error) = outcome {
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                interrupted += 1;
                assert!(opened.sparse.read_exact_at(0, &mut [0]).is_err());
            }
            drop(opened);
            if transaction::pending(&path).unwrap() {
                assert!(Vmdk::open_descriptor(&path, &extents).is_err());
                crate::writer_open::refuse_pending_open(
                    &path,
                    crate::ImageFormat::Vmdk,
                    Some(&extents),
                );
            }
            for extent in &extents {
                if transaction::pending(extent).unwrap() {
                    assert!(crate::VmdkWriter::open(extent).is_err());
                    assert!(Vmdk::open(Arc::new(crate::RawDisk::open(extent).unwrap())).is_err());
                }
            }
            let options = crate::WriterOpenOptions::default()
                .authorized_paths(extents.clone())
                .recovery_policy(crate::RecoveryPolicy::Recover);
            let reopened =
                crate::ImageWriter::open_with_options(&path, crate::ImageFormat::Vmdk, &options)
                    .unwrap();
            let mut actual = vec![0; 131072];
            reopened.read_exact_at(0, &mut actual).unwrap();
            let mut expected = vec![1; 65536];
            expected.extend(vec![2; 65536]);
            expected[17..36].fill(7);
            assert_eq!(actual, expected);
            assert!(!transaction::pending(&path).unwrap());
            for extent in &extents {
                assert!(!transaction::pending(extent).unwrap());
            }
        }
        assert!(
            interrupted >= 12,
            "all publication boundaries must be covered"
        );
    }
    #[test]
    fn late_participant_replacement_refuses_before_earlier_mutation() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        let opened = open(&path, &extents).unwrap();
        assert_eq!(
            opened
                .sparse
                .write_with_cut(17, &[7; 19], Some(0))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        drop(opened);
        let first = std::fs::read(&extents[0]).unwrap();
        let descriptor = std::fs::read(&path).unwrap();
        let last = std::fs::read(&extents[1]).unwrap();
        std::fs::rename(&extents[1], extents[1].with_extension("old")).unwrap();
        std::fs::write(&extents[1], last).unwrap();
        assert!(crate::VmdkWriter::open_descriptor(&path, &extents).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), descriptor);
        assert_eq!(std::fs::read(&extents[0]).unwrap(), first);
        assert!(transaction::pending(&path).unwrap());
    }
    #[test]
    fn exhausted_cumulative_budget_refuses_entire_call_before_cid() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        let opened = open(&path, &extents).unwrap();
        let usage = opened.sparse.budget.usage();
        let limits = opened.sparse.budget.limits();
        opened
            .sparse
            .budget
            .work(limits.work_items - usage.work_items - 1)
            .unwrap();
        let before = std::iter::once(&path)
            .chain(&extents)
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            opened
                .sparse
                .write_all_at(65529, &[7; 23])
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            opened.sparse.write_zeroes(65529, 23).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        for (path, bytes) in std::iter::once(&path).chain(&extents).zip(before) {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert!(!transaction::pending(path).unwrap());
        }
    }
    #[test]
    fn cid_is_persisted_before_payload_and_torn_payload_recovers() {
        let _boundary = crate::test_sync::writer_test();
        for cut in [0, 5] {
            let (_directory, path, extents) = fixture();
            let before = std::fs::read(&path).unwrap();
            let opened = open(&path, &extents).unwrap();
            let payload = opened.sparse.state.lock().unwrap().mappings[0][0];
            assert_eq!(
                opened
                    .sparse
                    .write_with_cut(17, &[7; 19], Some(cut))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Interrupted
            );
            let mut bytes = [0; 19];
            opened.sparse.files[1]
                .raw
                .read_exact_at(payload + 17, &mut bytes)
                .unwrap();
            assert_eq!(bytes, [1; 19]);
            if cut == 5 {
                assert_ne!(std::fs::read(&path).unwrap(), before);
            } else {
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
            // Simulate a torn payload containing independently valid old/new bytes.
            opened.sparse.files[1]
                .raw
                .write_all_at(payload + 17, &[7; 9])
                .unwrap();
            opened.sparse.files[1].raw.flush().unwrap();
            drop(opened);
            let writer = crate::VmdkWriter::open_descriptor(&path, &extents).unwrap();
            let mut actual = [0; 21];
            writer.read_exact_at(16, &mut actual).unwrap();
            assert_eq!(actual[0], 1);
            assert_eq!(&actual[1..20], &[7; 19]);
            assert_eq!(actual[20], 1);
        }
    }
    #[test]
    fn symlinked_participant_marker_is_never_followed_or_removed() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        let opened = open(&path, &extents).unwrap();
        assert!(opened.sparse.write_with_cut(17, &[7; 19], Some(0)).is_err());
        drop(opened);
        let marker = transaction::sidecar(&extents[1]);
        std::os::unix::fs::symlink(transaction::sidecar(&path), &marker).unwrap();
        let before = std::iter::once(&path)
            .chain(&extents)
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        assert!(crate::VmdkWriter::open_descriptor(&path, &extents).is_err());
        assert!(
            std::fs::symlink_metadata(marker)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        for (path, bytes) in std::iter::once(&path).chain(&extents).zip(before) {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    #[test]
    fn proposed_descriptor_must_resolve_to_retained_authorized_participants() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, path, extents) = fixture();
        let opened = open(&path, &extents).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let offset = bytes
            .windows(b"disk-s1.vmdk".len())
            .position(|bytes| bytes == b"disk-s1.vmdk")
            .unwrap() as u64;
        let source = Source {
            raw: opened.sparse.files[0].raw.clone(),
            size: bytes.len() as u64,
        };
        let mut records = vec![None; 3];
        records[0] = Some(Record {
            original_length: source.len(),
            final_length: source.len(),
            original_digest: transaction::digest_reader(&source).unwrap(),
            patches: vec![Patch {
                order: 0,
                offset,
                old: b"disk-s1.vmdk".to_vec(),
                new: b"disk-s2.vmdk".to_vec(),
            }],
        });
        let before = std::iter::once(&path)
            .chain(&extents)
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>();
        assert!(
            transaction_set::commit(&opened.sparse.files, records, None, &|sources| {
                validate_sources(sources, &opened.sparse.budget, &opened.sparse.files)
            })
            .is_err()
        );
        for (path, bytes) in std::iter::once(&path).chain(&extents).zip(before) {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert!(!transaction::pending(path).unwrap());
        }
    }
    #[test]
    fn foreign_marker_and_unchanged_in_place_mutation_refuse_without_cleanup() {
        let _boundary = crate::test_sync::writer_test();
        for foreign_marker in [false, true] {
            let (_directory, path, extents) = fixture();
            let opened = open(&path, &extents).unwrap();
            assert!(opened.sparse.write_with_cut(17, &[7; 19], Some(0)).is_err());
            drop(opened);
            if foreign_marker {
                std::fs::write(transaction::sidecar(&extents[1]), b"foreign").unwrap();
            } else {
                let mut bytes = std::fs::read(&extents[1]).unwrap();
                *bytes.last_mut().unwrap() ^= 1;
                std::fs::write(&extents[1], bytes).unwrap();
            }
            let first = std::fs::read(&extents[0]).unwrap();
            let descriptor = std::fs::read(&path).unwrap();
            assert!(crate::VmdkWriter::open_descriptor(&path, &extents).is_err());
            assert_eq!(std::fs::read(&extents[0]).unwrap(), first);
            assert_eq!(std::fs::read(&path).unwrap(), descriptor);
            if foreign_marker {
                assert_eq!(
                    std::fs::read(transaction::sidecar(&extents[1])).unwrap(),
                    b"foreign"
                );
            }
        }
    }
}
