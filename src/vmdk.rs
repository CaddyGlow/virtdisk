//! Bounded hosted sparse VMDK reader with explicitly authorized parent chains.
use crate::{CacheReservation, ParserLimits, ReadAt, ReadBudget, ReadContext, check_range};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
fn unsupported(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, s)
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn sector(v: u64) -> io::Result<u64> {
    v.checked_mul(512)
        .ok_or_else(|| invalid("VMDK sector overflow"))
}
fn claim(owners: &mut BTreeMap<u64, u64>, start: u64, length: u64, file: u64) -> io::Result<()> {
    check_range(start, length, file)?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid("VMDK ownership overflow"))?;
    if owners
        .range(..end)
        .next_back()
        .is_some_and(|(_, e)| *e > start)
    {
        return Err(invalid("overlapping VMDK ownership"));
    }
    owners.insert(start, end);
    Ok(())
}
#[cfg(target_os = "linux")]
#[path = "vmdk_pinned.rs"]
mod pinned;
#[cfg(target_os = "linux")]
pub(crate) use pinned::{
    DependencyExtent, DependencyExtentKind, DependencyNode, PinnedParentGraph,
};
struct OpenedSource {
    source: Arc<dyn ReadAt>,
    identity: Option<same_file::Handle>,
}
trait SourceFactory {
    fn open(&mut self, path: &Path, budget: &ReadBudget) -> io::Result<OpenedSource>;
    fn allow_pending(&self) -> bool {
        false
    }
    fn node(&mut self, _path: &Path, _link: &Link, _hosted: bool) -> io::Result<()> {
        Ok(())
    }
    fn parent(&mut self, _child: &Path, _parent: &Path) -> io::Result<()> {
        Ok(())
    }
    fn extent(
        &mut self,
        _node: &Path,
        _path: &Path,
        _sparse: bool,
        _start: u64,
        _length: u64,
        _offset: u64,
    ) -> io::Result<()> {
        Ok(())
    }
}
struct OrdinaryFactory;
impl SourceFactory for OrdinaryFactory {
    fn open(&mut self, path: &Path, _budget: &ReadBudget) -> io::Result<OpenedSource> {
        let raw = crate::RawDisk::open(path)?;
        let identity = raw.identity()?;
        Ok(OpenedSource {
            source: Arc::new(raw),
            identity: Some(identity),
        })
    }
}
struct Authorized {
    paths: BTreeSet<PathBuf>,
    _cache: Vec<CacheReservation>,
}
impl std::ops::Deref for Authorized {
    type Target = BTreeSet<PathBuf>;
    fn deref(&self) -> &Self::Target {
        &self.paths
    }
}
fn authorize(paths: &[PathBuf], budget: &ReadBudget) -> io::Result<Authorized> {
    let mut authorized = BTreeSet::new();
    let mut cache = Vec::new();
    for path in paths {
        budget.work(1)?;
        let canonical = path.canonicalize()?;
        let size = canonical.as_os_str().len() as u64 + 128;
        budget.metadata(size)?;
        cache.push(budget.cache(size)?);
        authorized.insert(canonical);
    }
    Ok(Authorized {
        paths: authorized,
        _cache: cache,
    })
}
struct Link {
    cid: u32,
    parent_cid: u32,
    hint: Option<String>,
    sectors: u64,
    extent_count: u64,
    profile: String,
    sparse_extents: bool,
}
impl Link {
    fn parse(text: &str, budget: &ReadBudget) -> io::Result<Self> {
        let mut properties = crate::vmdk_descriptor::Properties::default();
        let mut cid = None;
        let mut parent_cid = None;
        let mut hint = None;
        let mut version = false;
        let mut profile = None;
        let mut sectors = 0u64;
        let mut extent_count = 0u64;
        let mut sparse_extents = true;
        for line in text.lines() {
            budget.work(1)?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, field)) = crate::vmdk_descriptor::property(line, 0) else {
                let extent = crate::vmdk_descriptor::extent(line, 0)?;
                if extent.access != "RW" {
                    return Err(unsupported("unsupported VMDK descriptor syntax"));
                }
                let capacity = crate::vmdk_descriptor::number(extent.count.text)?;
                if capacity == 0 {
                    return Err(invalid("empty VMDK extent"));
                }
                let kind = extent.kind;
                sparse_extents &= kind == "SPARSE";
                let name = extent.name;
                if name.is_empty()
                    || name.contains([':', '\\'])
                    || name.chars().any(char::is_control)
                {
                    return Err(unsupported("unsupported VMDK extent name"));
                }
                if kind == "SPARSE" && !extent.tail.is_empty() {
                    return Err(invalid("unexpected sparse extent offset"));
                }
                sectors = sectors
                    .checked_add(capacity)
                    .ok_or_else(|| invalid("VMDK capacity overflow"))?;
                extent_count += 1;
                continue;
            };
            let duplicate = properties.duplicate(key);
            let value = field.text;
            match key {
                "CID" | "parentCID" => {
                    let field = if key.trim() == "CID" {
                        &mut cid
                    } else {
                        &mut parent_cid
                    };
                    if duplicate || crate::vmdk_descriptor::hexadecimal(value).is_err() {
                        return Err(invalid("invalid or duplicate VMDK CID"));
                    }
                    *field = Some(crate::vmdk_descriptor::hexadecimal(value)?);
                }
                "parentFileNameHint" => {
                    if duplicate {
                        return Err(invalid("duplicate VMDK parent hint"));
                    }
                    let name = value
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .ok_or_else(|| invalid("invalid VMDK parent hint"))?;
                    if name.is_empty()
                        || name.contains([':', '\\', '"'])
                        || name.chars().any(char::is_control)
                    {
                        return Err(unsupported("unsupported VMDK parent hint"));
                    }
                    hint = Some(name.to_owned());
                }
                "version" => {
                    if duplicate || value != "1" {
                        return Err(invalid("invalid VMDK descriptor version"));
                    }
                    version = true;
                }
                "createType" => {
                    if duplicate {
                        return Err(invalid("duplicate VMDK createType"));
                    }
                    profile = Some(value.to_owned());
                }
                k if k.starts_with("ddb.") => {}
                _ => return Err(unsupported("unsupported VMDK descriptor property")),
            }
        }
        if !version || profile.is_none() || extent_count == 0 {
            return Err(invalid("incomplete VMDK chain descriptor"));
        }
        Ok(Self {
            cid: cid.ok_or_else(|| invalid("missing VMDK CID"))?,
            parent_cid: parent_cid.ok_or_else(|| invalid("missing VMDK parentCID"))?,
            hint,
            sectors,
            extent_count,
            profile: profile.unwrap(),
            sparse_extents,
        })
    }
}

/// Read-only uncompressed hosted sparse VMDK v1/v2 and authorized flat/sparse descriptors.
///
/// Parent chains require `open_chain` and explicit ancestor/extent authorization.
/// Compressed, footer and dirty native profiles fail closed.
/// Underlying sources must remain immutable for this reader's lifetime.
pub struct Vmdk {
    container_set_size: u64,
    source: Arc<dyn ReadAt>,
    length: u64,
    grain: u64,
    entries: Vec<u32>,
    _cache: CacheReservation,
    extents: Vec<(u64, Arc<dyn ReadAt>)>,
    parent: Option<Arc<dyn ReadAt>>,
    cid: Option<u32>,
    _identity_cache: Option<CacheReservation>,
}
impl Vmdk {
    pub(crate) fn container_sizes(&self) -> (u64, u64) {
        (self.source.len(), self.container_set_size)
    }

    pub(crate) fn content_id(&self) -> Option<u32> {
        self.cid
    }
    pub(crate) fn writer_zero_mask(&self) -> Vec<bool> {
        self.entries.iter().map(|e| *e == 1).collect()
    }
    pub(crate) fn resolve_writer_parent(
        source: Arc<dyn ReadAt>,
        path: &Path,
        authorized_paths: &[PathBuf],
        child_identity: same_file::Handle,
    ) -> io::Result<(Option<Arc<Vmdk>>, same_file::Handle)> {
        let budget = ReadBudget::new(ParserLimits::default())?;
        let link = Self::hosted_link(source, &budget)?;
        if link.parent_cid == u32::MAX {
            if link.hint.is_some() {
                return Err(invalid("standalone VMDK has parent hint"));
            }
            return Ok((None, child_identity));
        }
        let hint = link
            .hint
            .ok_or_else(|| invalid("missing VMDK parent hint"))?;
        let parent_path = path
            .parent()
            .ok_or_else(|| invalid("missing VMDK directory"))?
            .join(hint)
            .canonicalize()?;
        let authorized = authorize(authorized_paths, &budget)?;
        if !authorized.contains(&parent_path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VMDK parent requires explicit authorization",
            ));
        }
        let mut identities = vec![child_identity];
        let parent = Self::chain_node(
            &parent_path,
            &authorized,
            &budget,
            &mut identities,
            1,
            &mut OrdinaryFactory,
        )?;
        if parent.cid != Some(link.parent_cid) {
            return Err(invalid("VMDK parent CID mismatch"));
        }
        Ok((Some(Arc::new(parent)), identities.remove(0)))
    }
    fn hosted_link(source: Arc<dyn ReadAt>, budget: &ReadBudget) -> io::Result<Link> {
        let mut header = [0; 512];
        source.read_exact_at(0, &mut header)?;
        if &header[..4] != b"KDMV" {
            return Err(unsupported("VMDK writer requires hosted sparse child"));
        }
        let size = sector(u64le(&header, 36))?;
        if size == 0 {
            return Err(invalid("VMDK chain requires bounded descriptor"));
        }
        budget.attribute(size)?;
        budget.metadata(size)?;
        let _scratch = budget.cache(size)?;
        let mut bytes = vec![0; size as usize];
        source.read_exact_at(sector(u64le(&header, 28))?, &mut bytes)?;
        let link = Link::parse(crate::vmdk_descriptor::text(&bytes)?, budget)?;
        if link.sectors != u64le(&header, 12)
            || link.extent_count != 1
            || !link.sparse_extents
            || link.profile != "\"monolithicSparse\""
        {
            return Err(invalid(
                "VMDK embedded descriptor capacity or profile mismatch",
            ));
        }
        Ok(link)
    }
    pub(crate) fn open_parented(
        source: Arc<dyn ReadAt>,
        parent: Option<Arc<Vmdk>>,
    ) -> io::Result<Self> {
        let Some(parent) = parent else {
            return Self::open(source);
        };
        let budget = parent
            .budget()
            .ok_or_else(|| invalid("VMDK parent lacks shared budget"))?;
        let source = budget.reader(source);
        let link = Self::hosted_link(source.clone(), &budget)?;
        if Some(link.parent_cid) != parent.cid || link.parent_cid == u32::MAX || link.hint.is_none()
        {
            return Err(invalid("VMDK parent CID mismatch"));
        }
        let mut disk = Self::parse_mode(source, &budget, true)?;
        if disk.length != parent.length {
            return Err(invalid("VMDK parent capacity mismatch"));
        }
        disk.parent = Some(parent);
        disk.cid = Some(link.cid);
        Ok(disk)
    }

    /// Open a hosted VMDK chain using only explicitly authorized ancestors and extents.
    /// Parent CIDs and capacities must match. All opened files must remain immutable.
    pub fn open_chain(path: impl AsRef<Path>, authorized_paths: &[PathBuf]) -> io::Result<Self> {
        Self::open_chain_with_limits(path, authorized_paths, ParserLimits::default())
    }
    /// Open a chain with one shared metadata, cache, work and depth budget.
    pub fn open_chain_with_limits(
        path: impl AsRef<Path>,
        authorized_paths: &[PathBuf],
        limits: ParserLimits,
    ) -> io::Result<Self> {
        Self::open_chain_with_budget(path, authorized_paths, ReadBudget::new(limits)?)
    }
    pub(crate) fn open_chain_with_budget(
        path: impl AsRef<Path>,
        authorized_paths: &[PathBuf],
        budget: ReadBudget,
    ) -> io::Result<Self> {
        let authorized = authorize(authorized_paths, &budget)?;
        let mut identities = Vec::new();
        Self::chain_node(
            &path.as_ref().canonicalize()?,
            &authorized,
            &budget,
            &mut identities,
            0,
            &mut OrdinaryFactory,
        )
    }
    fn chain_node(
        path: &Path,
        authorized: &BTreeSet<PathBuf>,
        budget: &ReadBudget,
        identities: &mut Vec<same_file::Handle>,
        depth: u64,
        factory: &mut dyn SourceFactory,
    ) -> io::Result<Self> {
        budget.recursion(u128::from(depth) + 1, 64)?;
        if crate::transaction::pending(path)? {
            return Err(invalid("VMDK has a pending transaction"));
        }
        budget.work(1)?;
        let opened = factory.open(path, budget)?;
        let identity = opened
            .identity
            .ok_or_else(|| invalid("missing VMDK discovery identity"))?;
        budget.work(identities.len() as u64)?;
        if identities.contains(&identity) {
            return Err(invalid("VMDK chain cycle or file alias"));
        }
        budget.metadata(128)?;
        let identity_cache = budget.cache(128)?;
        identities.push(identity);
        let source = budget.reader(opened.source);
        let mut header = [0; 512];
        source.read_exact_at(0, &mut header[..4])?;
        let hosted = &header[..4] == b"KDMV";
        if hosted {
            source.read_exact_at(4, &mut header[4..])?;
        }
        let (offset, size) = if hosted {
            (sector(u64le(&header, 28))?, sector(u64le(&header, 36))?)
        } else {
            (0, source.len())
        };
        if size == 0 {
            return Err(invalid("VMDK chain requires bounded descriptor"));
        }
        budget.attribute(size)?;
        check_range(offset, size, source.len())?;
        budget.metadata(size)?;
        let _scratch = budget.cache(size)?;
        let mut bytes =
            vec![0; usize::try_from(size).map_err(|_| invalid("VMDK descriptor too large"))?];
        source.read_exact_at(offset, &mut bytes)?;
        let text = crate::vmdk_descriptor::text(&bytes)?;
        let link = Link::parse(text, budget)?;
        if hosted
            && (link.sectors != u64le(&header, 12)
                || link.extent_count != 1
                || !link.sparse_extents
                || link.profile != "\"monolithicSparse\"")
        {
            return Err(invalid(
                "VMDK embedded descriptor capacity or profile mismatch",
            ));
        }
        factory.node(path, &link, hosted)?;
        let parent: Option<Arc<dyn ReadAt>> = if link.parent_cid != u32::MAX {
            let hint = link
                .hint
                .as_ref()
                .ok_or_else(|| invalid("VMDK parent hint missing"))?;
            let parent_path = path
                .parent()
                .ok_or_else(|| invalid("missing VMDK directory"))?
                .join(hint)
                .canonicalize()?;
            if !authorized.contains(&parent_path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "VMDK parent requires explicit authorization",
                ));
            }
            let ancestor = Self::chain_node(
                &parent_path,
                authorized,
                budget,
                identities,
                depth + 1,
                factory,
            )?;
            if ancestor.cid != Some(link.parent_cid) {
                return Err(invalid("VMDK parent CID mismatch"));
            }
            factory.parent(path, &parent_path)?;
            Some(Arc::new(ancestor))
        } else {
            if link.hint.is_some() {
                return Err(invalid("standalone VMDK has parent hint"));
            }
            None
        };
        let mut disk = if hosted {
            Self::parse_mode(source, budget, true)?
        } else {
            Self::parse_descriptor(
                source,
                path,
                budget,
                authorized,
                identities,
                parent.clone(),
                factory,
            )?
        };
        if let Some(parent) = &parent
            && parent.len() != disk.length
        {
            return Err(invalid("VMDK parent capacity mismatch"));
        }
        disk.cid = Some(link.cid);
        disk._identity_cache = Some(identity_cache);
        disk.parent = parent;
        Ok(disk)
    }
    /// Canonical direct parent path retained by an explicitly authorized chain open.
    pub fn resolved_parent_path(&self) -> Option<PathBuf> {
        self.parent
            .as_ref()
            .and_then(|parent| parent.context().container)
    }
    /// Whether this opened image resolves an authorized parent.
    pub fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    pub(crate) fn info_profile(&self) -> (Option<u64>, bool) {
        (
            if self.extents.is_empty() {
                Some(self.grain)
            } else {
                None
            },
            !self.extents.is_empty(),
        )
    }
    /// Open with default bounded parser limits.
    pub fn open(source: Arc<dyn ReadAt>) -> io::Result<Self> {
        Self::open_with_limits(source, ParserLimits::default())
    }
    /// Open with caller-tightened parser limits.
    pub fn open_with_limits(source: Arc<dyn ReadAt>, limits: ParserLimits) -> io::Result<Self> {
        if let Some(path) = source.context().container
            && crate::transaction::pending(&path)?
        {
            return Err(invalid(
                "VMDK has a pending transaction; reopen with VmdkWriter to recover",
            ));
        }
        let budget = ReadBudget::new(limits)?;
        let source = budget.reader(source);
        let context = source.context();
        Self::parse(source, &budget).map_err(|e| context.error("parse VMDK", e))
    }
    /// Open a standalone descriptor using only explicitly authorized extent files.
    ///
    /// Relative extent names resolve against the descriptor directory. Canonical
    /// identities must appear in `authorized_extent_paths`. Repeated extent files,
    /// parents, devices and network names are rejected. Sources must stay immutable.
    pub fn open_descriptor(
        path: impl AsRef<Path>,
        authorized_extent_paths: &[PathBuf],
    ) -> io::Result<Self> {
        Self::open_descriptor_with_limits(path, authorized_extent_paths, ParserLimits::default())
    }
    /// Open an authorized flat/sparse descriptor with caller-tightened budgets.
    pub fn open_descriptor_with_limits(
        path: impl AsRef<Path>,
        authorized_extent_paths: &[PathBuf],
        limits: ParserLimits,
    ) -> io::Result<Self> {
        let budget = ReadBudget::new(limits)?;
        let path = path.as_ref().canonicalize()?;
        let descriptor = crate::RawDisk::open(&path)?;
        let mut identities = vec![descriptor.identity()?];
        let source = budget.reader(Arc::new(descriptor));
        let authorized = authorize(authorized_extent_paths, &budget)?;
        Self::parse_descriptor(
            source,
            &path,
            &budget,
            &authorized,
            &mut identities,
            None,
            &mut OrdinaryFactory,
        )
    }
    fn parse_descriptor(
        source: Arc<dyn ReadAt>,
        path: &Path,
        budget: &ReadBudget,
        authorized: &BTreeSet<PathBuf>,
        identities: &mut Vec<same_file::Handle>,
        backing: Option<Arc<dyn ReadAt>>,
        factory: &mut dyn SourceFactory,
    ) -> io::Result<Self> {
        if !factory.allow_pending() && crate::transaction::pending(path)? {
            return Err(invalid("VMDK descriptor has a pending transaction"));
        }
        budget.attribute(source.len())?;
        budget.metadata(source.len())?;
        let _descriptor_cache = budget.cache(source.len())?;
        let mut bytes = vec![0; source.len() as usize];
        source.read_exact_at(0, &mut bytes)?;
        let text = crate::vmdk_descriptor::text(&bytes)?;
        let mut used = BTreeSet::new();
        let mut extent_scratch = Vec::new();
        let mut extents = Vec::new();
        let mut length = 0u64;
        let mut container_set_size = source.len();
        let mut properties = crate::vmdk_descriptor::Properties::default();
        let mut parent = false;
        let mut version = false;
        let mut cid = false;
        let mut create_type = false;
        let mut profile = "";
        let mut extent_kind = "";
        for line in text.lines() {
            budget.work(1)?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, field)) = crate::vmdk_descriptor::property(line, 0) {
                let duplicate = properties.duplicate(key);
                let value = field.text;
                match key {
                    "version" => {
                        if duplicate || value != "1" {
                            return Err(unsupported("unsupported VMDK descriptor version"));
                        }
                        version = true;
                    }
                    "CID" => {
                        if duplicate
                            || value.is_empty()
                            || value.len() > 8
                            || crate::vmdk_descriptor::hexadecimal(value).is_err()
                        {
                            return Err(invalid("invalid VMDK CID"));
                        }
                        cid = true;
                    }
                    "parentCID" => {
                        if duplicate || (value != "ffffffff" && backing.is_none()) {
                            return Err(unsupported("VMDK parent requires authorization"));
                        }
                        parent = true;
                    }
                    "parentFileNameHint" => {
                        if backing.is_none() {
                            return Err(unsupported("VMDK parent requires authorization"));
                        }
                    }
                    "createType" => {
                        if duplicate {
                            return Err(invalid("duplicate VMDK createType"));
                        }
                        create_type = true;
                        profile = value;
                        if ![
                            "\"monolithicFlat\"",
                            "\"twoGbMaxExtentFlat\"",
                            "\"twoGbMaxExtentSparse\"",
                            "\"monolithicSparse\"",
                        ]
                        .contains(&value)
                        {
                            return Err(unsupported("unsupported VMDK extent profile"));
                        }
                    }
                    k if k.starts_with("ddb.") => {}
                    _ => return Err(unsupported("unsupported VMDK descriptor property")),
                }
                continue;
            }
            let extent = crate::vmdk_descriptor::extent(line, 0)?;
            if extent.access != "RW" {
                return Err(unsupported("unsupported VMDK extent access"));
            }
            if extent_kind.is_empty() {
                extent_kind = extent.kind;
            } else if extent_kind != extent.kind {
                return Err(unsupported("mixed VMDK extent profiles"));
            }
            let size = crate::vmdk_descriptor::sectors(extent.count.text)?;
            if size == 0 {
                return Err(invalid("empty VMDK extent"));
            }
            let name = extent.name;
            if name.is_empty()
                || name.contains(':')
                || name.contains('\\')
                || name.chars().any(char::is_control)
            {
                return Err(unsupported("unsupported VMDK extent name"));
            }
            let extent_path = path
                .parent()
                .ok_or_else(|| invalid("missing descriptor directory"))?
                .join(name)
                .canonicalize()?;
            if !authorized.contains(&extent_path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "VMDK extent requires explicit authorization",
                ));
            }
            if !used.insert(extent_path.clone()) {
                return Err(unsupported("repeated VMDK extent file"));
            }
            if !factory.allow_pending() && crate::transaction::pending(&extent_path)? {
                return Err(invalid("VMDK extent has a pending transaction"));
            }
            budget.metadata(128)?;
            extent_scratch.push(budget.cache(128)?);
            let opened = factory.open(&extent_path, budget)?;
            if let Some(identity) = opened.identity {
                budget.work(identities.len() as u64)?;
                if identities.contains(&identity) {
                    return Err(unsupported("VMDK extent aliases another opened file"));
                }
                identities.push(identity);
            }
            let extent_source = budget.reader(opened.source);
            container_set_size = container_set_size
                .checked_add(extent_source.len())
                .ok_or_else(|| invalid("VMDK container set size overflow"))?;
            let tail = extent.tail;
            let reader: Arc<dyn ReadAt> = match extent.kind {
                "FLAT" => {
                    let offset = sector(
                        tail.parse()
                            .map_err(|_| invalid("invalid flat extent offset"))?,
                    )?;
                    Arc::new(crate::DiskView::new(extent_source, offset, size)?)
                }
                "SPARSE" => {
                    if !tail.is_empty() {
                        return Err(invalid("unexpected sparse extent offset"));
                    }
                    let mut disk = Self::parse(extent_source, budget)?;
                    if let Some(parent) = &backing {
                        disk.parent = Some(Arc::new(crate::DiskView::new(
                            parent.clone(),
                            length,
                            size,
                        )?));
                    }
                    if disk.len() != size {
                        return Err(invalid("sparse extent capacity mismatch"));
                    }
                    Arc::new(disk)
                }
                _ => return Err(unsupported("unsupported VMDK extent type")),
            };
            let physical_offset = if extent.kind == "FLAT" {
                sector(
                    tail.parse()
                        .map_err(|_| invalid("invalid flat extent offset"))?,
                )?
            } else {
                0
            };
            factory.extent(
                path,
                &extent_path,
                extent.kind == "SPARSE",
                length,
                size,
                physical_offset,
            )?;
            extents.push((length, reader));
            length = length
                .checked_add(size)
                .ok_or_else(|| invalid("VMDK descriptor capacity overflow"))?;
        }
        if !parent || !version || !cid || !create_type || extents.is_empty() {
            return Err(invalid("incomplete VMDK descriptor"));
        }
        let flat = profile.ends_with("Flat\"");
        if (flat && extent_kind != "FLAT")
            || (!flat && extent_kind != "SPARSE")
            || (profile.starts_with("\"monolithic") && extents.len() != 1)
        {
            return Err(invalid("VMDK createType does not match extents"));
        }
        let cache = budget.cache(
            (extents.len() as u64)
                .checked_mul(128)
                .ok_or_else(|| invalid("VMDK extent cache overflow"))?,
        )?;
        Ok(Self {
            container_set_size,
            source,
            length,
            grain: 0,
            entries: Vec::new(),
            _cache: cache,
            extents,
            parent: backing,
            cid: None,
            _identity_cache: None,
        })
    }
    pub(crate) fn writer_mappings(&self) -> io::Result<Vec<u64>> {
        if self.grain != 65536 || !self.extents.is_empty() {
            return Err(unsupported(
                "VMDK writer requires 64 KiB hosted sparse grains",
            ));
        }
        Ok(self
            .entries
            .iter()
            .map(|e| if *e <= 1 { 0 } else { u64::from(*e) * 512 })
            .collect())
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn open_with_budget(
        source: Arc<dyn ReadAt>,
        budget: &ReadBudget,
    ) -> io::Result<Self> {
        Self::parse(budget.reader(source), budget)
    }
    fn parse(source: Arc<dyn ReadAt>, budget: &ReadBudget) -> io::Result<Self> {
        Self::parse_mode(source, budget, false)
    }
    fn parse_mode(
        source: Arc<dyn ReadAt>,
        budget: &ReadBudget,
        allow_parent: bool,
    ) -> io::Result<Self> {
        if let Some(path) = source.context().container
            && crate::transaction::pending(&path)?
        {
            return Err(invalid("VMDK extent has a pending transaction"));
        }
        budget.metadata(512)?;
        let mut h = [0; 512];
        source.read_exact_at(0, &mut h)?;
        if &h[..4] != b"KDMV" {
            return Err(invalid("invalid VMDK magic"));
        }
        let flags = u32le(&h, 8);
        if ![1, 2].contains(&u32le(&h, 4)) || flags & !7 != 0 || h[72] != 0 || h[77..79] != [0, 0] {
            return Err(unsupported("unsupported VMDK profile"));
        }
        if flags & 1 != 0 && h[73..77] != [10, 32, 13, 10] {
            return Err(invalid("invalid VMDK newline sentinel"));
        }
        let length = sector(u64le(&h, 12))?;
        let grain = sector(u64le(&h, 20))?;
        let gtes = u64::from(u32le(&h, 44));
        if length == 0
            || grain == 0
            || !grain.is_power_of_two()
            || gtes == 0
            || !gtes.is_power_of_two()
        {
            return Err(invalid("invalid VMDK geometry"));
        }
        let overhead = sector(u64le(&h, 64))?;
        if overhead < 512 || overhead > source.len() {
            return Err(invalid("invalid VMDK overhead"));
        }
        let count = length.div_ceil(grain);
        let directories = count.div_ceil(gtes);
        let map_bytes = count
            .checked_mul(4)
            .ok_or_else(|| invalid("VMDK mapping overflow"))?;
        budget.metadata(map_bytes)?;
        budget.work(count)?;
        let cache = budget.cache(
            count
                .checked_mul(128)
                .ok_or_else(|| invalid("VMDK cache overflow"))?,
        )?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(
                usize::try_from(count).map_err(|_| invalid("VMDK mapping too large"))?,
            )
            .map_err(|_| invalid("VMDK allocation failed"))?;
        let mut owners = BTreeMap::new();
        claim(&mut owners, 0, 512, source.len())?;
        let desc_offset = sector(u64le(&h, 28))?;
        let desc_size = sector(u64le(&h, 36))?;
        if (desc_offset == 0) != (desc_size == 0) {
            return Err(invalid("invalid VMDK descriptor range"));
        }
        if desc_size != 0 {
            budget.metadata(desc_size)?;
            budget.attribute(desc_size)?;
            claim(&mut owners, desc_offset, desc_size, overhead)?;
            let _descriptor_cache = budget.cache(desc_size)?;
            let mut desc = vec![0; desc_size as usize];
            source.read_exact_at(desc_offset, &mut desc)?;
            let desc = crate::vmdk_descriptor::text(&desc)?;
            // QEMU split sparse extents reserve an entirely zero descriptor area.
            // Their external descriptor owns the linkage; this is an absent descriptor.
            let mut parent = desc.is_empty();
            for line in desc.lines() {
                let line = line.trim();
                if line.starts_with('#') {
                    continue;
                }
                if let Some((key, field)) = crate::vmdk_descriptor::property(line, 0) {
                    let value = field.text;
                    match key {
                        "parentCID" => {
                            if value.trim() != "ffffffff" && !allow_parent {
                                return Err(unsupported("VMDK parent requires authorization"));
                            }
                            parent = true;
                        }
                        "parentFileNameHint" if !allow_parent => {
                            return Err(unsupported("VMDK parent requires authorization"));
                        }
                        "createType" if value.trim() != "\"monolithicSparse\"" => {
                            return Err(unsupported("unsupported VMDK descriptor profile"));
                        }
                        _ => {}
                    }
                }
            }
            if !parent {
                return Err(invalid("VMDK descriptor lacks parentCID"));
            }
        }
        if (flags & 2 != 0) != (u64le(&h, 48) != 0) {
            return Err(invalid("VMDK redundant directory flag mismatch"));
        }
        for (pass, directory_sector) in [u64le(&h, 56), u64le(&h, 48)].into_iter().enumerate() {
            if pass == 1 && directory_sector == 0 {
                continue;
            }
            let gd = sector(directory_sector)?;
            let gd_bytes = directories
                .checked_mul(4)
                .ok_or_else(|| invalid("VMDK directory overflow"))?;
            claim(&mut owners, gd, gd_bytes, overhead)?;
            budget.metadata(gd_bytes)?;
            let _directory_cache = budget.cache(gd_bytes)?;
            let mut directory = vec![0; gd_bytes as usize];
            source.read_exact_at(gd, &mut directory)?;
            for d in 0..directories {
                let table_sector = u64::from(u32le(&directory, d as usize * 4));
                let remaining = (count - d * gtes).min(gtes);
                if table_sector == 0 {
                    if pass == 0 {
                        entries.extend(std::iter::repeat_n(0, remaining as usize));
                    } else if entries[(d * gtes) as usize..(d * gtes + remaining) as usize]
                        .iter()
                        .any(|e| *e != 0)
                    {
                        return Err(invalid("VMDK redundant mapping mismatch"));
                    }
                    continue;
                }
                let table = sector(table_sector)?;
                let bytes = gtes
                    .checked_mul(4)
                    .ok_or_else(|| invalid("VMDK table overflow"))?;
                budget.metadata(bytes)?;
                budget.work(gtes)?;
                budget.attribute(bytes)?;
                claim(&mut owners, table, bytes, overhead)?;
                let _table_cache = budget.cache(bytes)?;
                let mut data = vec![0; bytes as usize];
                source.read_exact_at(table, &mut data)?;
                for i in 0..gtes {
                    let entry = u32le(&data, i as usize * 4);
                    if i >= remaining {
                        if entry != 0 {
                            return Err(invalid("VMDK mapping beyond capacity"));
                        }
                        continue;
                    }
                    if entry == 1 && flags & 4 == 0 {
                        return Err(invalid("VMDK zero grain without flag"));
                    }
                    if entry > 1 && pass == 0 {
                        if sector(u64::from(entry))? < overhead {
                            return Err(invalid("VMDK data inside metadata area"));
                        }
                        claim(&mut owners, sector(u64::from(entry))?, grain, source.len())?;
                    }
                    if pass == 0 {
                        entries.push(entry);
                    } else if entries[(d * gtes + i) as usize] != entry {
                        return Err(invalid("VMDK redundant mapping mismatch"));
                    }
                }
            }
        }
        if overhead > source.len() || owners.iter().any(|(s, e)| *s < overhead && *e > overhead) {
            return Err(invalid("invalid VMDK overhead"));
        }
        Ok(Self {
            container_set_size: source.len(),
            source,
            length,
            grain,
            entries,
            _cache: cache,
            extents: Vec::new(),
            parent: None,
            cid: None,
            _identity_cache: None,
        })
    }
}
impl ReadAt for Vmdk {
    fn len(&self) -> u64 {
        self.length
    }
    fn visit_extents(
        &self,
        visitor: &mut dyn FnMut(crate::DiskExtent) -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.extents.is_empty() {
            for (start, reader) in &self.extents {
                if let Some(budget) = self.budget() {
                    budget.work(1)?;
                }
                reader.visit_extents(&mut |mut extent| {
                    if let Some(budget) = self.budget() {
                        budget.work(1)?;
                    }
                    extent.offset = extent
                        .offset
                        .checked_add(*start)
                        .ok_or_else(|| invalid("VMDK extent offset overflow"))?;
                    if extent.kind == crate::ExtentKind::Unknown {
                        extent.kind = crate::ExtentKind::Allocated;
                    }
                    visitor(extent)
                })?;
            }
            return Ok(());
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if let Some(budget) = self.budget() {
                budget.work(1)?;
            }
            let offset = index as u64 * self.grain;
            visitor(crate::DiskExtent {
                offset,
                length: (self.length - offset).min(self.grain),
                kind: if *entry == 0 && self.parent.is_some() {
                    crate::ExtentKind::Inherited
                } else if *entry <= 1 {
                    crate::ExtentKind::Zero
                } else {
                    crate::ExtentKind::Allocated
                },
            })?;
        }
        Ok(())
    }
    fn context(&self) -> ReadContext {
        self.source.context()
    }
    fn budget(&self) -> Option<ReadBudget> {
        self.source.budget()
    }
    fn read_exact_at(&self, mut offset: u64, mut out: &mut [u8]) -> io::Result<()> {
        check_range(offset, out.len() as u64, self.length)?;
        if !self.extents.is_empty() {
            while !out.is_empty() {
                if let Some(budget) = self.budget() {
                    budget.work(1)?;
                }
                let index = self.extents.partition_point(|(start, _)| *start <= offset) - 1;
                let (start, reader) = &self.extents[index];
                let local = offset - *start;
                let take = (reader.len() - local).min(out.len() as u64) as usize;
                reader.read_exact_at(local, &mut out[..take])?;
                offset += take as u64;
                out = &mut out[take..];
            }
            return Ok(());
        }
        while !out.is_empty() {
            if let Some(budget) = self.budget() {
                budget.work(1)?;
            }
            let within = offset % self.grain;
            let take = (self.grain - within).min(out.len() as u64) as usize;
            let entry = self.entries[(offset / self.grain) as usize];
            if entry == 0
                && let Some(parent) = &self.parent
            {
                parent.read_exact_at(offset, &mut out[..take])?;
            } else if entry <= 1 {
                out[..take].fill(0);
            } else {
                self.source
                    .read_exact_at(sector(u64::from(entry))? + within, &mut out[..take])?;
            }
            offset += take as u64;
            out = &mut out[take..];
        }
        Ok(())
    }
}
