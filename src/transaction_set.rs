//! Linux bounded multi-file transactions, validated as a complete set.
use crate::{
    RawWriter, ReadAt,
    transaction::{self, Record},
    vmdk::{DependencyExtent, DependencyExtentKind, DependencyNode, PinnedParentGraph},
};
use sha2::{Digest, Sha256};
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{MetadataExt, OpenOptionsExt},
};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

const MAGIC: &[u8; 8] = b"VDTXSET1";
const BACKED_MAGIC: &[u8; 8] = b"VDTXSET2";
const MARKER: &[u8; 8] = b"VDTXPAR1";
const LIMIT: usize = 4 * 1024 * 1024;
const MAX_FILES: usize = 257;

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid hosted image transaction set",
    )
}
fn sync_parent(path: &Path) -> io::Result<()> {
    std::fs::File::open(path.parent().ok_or_else(invalid)?)?.sync_all()
}

pub(super) struct File {
    pub(super) path: PathBuf,
    pub(super) raw: Arc<RawWriter>,
    pub(super) budget: crate::ReadBudget,
}
impl File {
    fn verify_path(&self) -> io::Result<(u64, u64)> {
        self.raw.require_single_link_for_journal()?;
        let identity = self.raw.file_identity()?.ok_or_else(invalid)?;
        let metadata = std::fs::symlink_metadata(&self.path)?;
        if !metadata.is_file()
            || (metadata.dev(), metadata.ino()) != identity
            || metadata.len() != self.raw.len()
            || self.path.canonicalize()? != self.path
        {
            return Err(invalid());
        }
        Ok(identity)
    }
}
struct Source {
    raw: Arc<RawWriter>,
    length: u64,
}
impl ReadAt for Source {
    fn len(&self) -> u64 {
        self.length
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        crate::check_range(offset, bytes.len() as u64, self.length)?;
        self.raw.read_exact_at(offset, bytes)
    }
}

#[derive(Clone)]
struct Participant {
    path: PathBuf,
    identity: (u64, u64),
    length: u64,
    digest: [u8; 32],
    record: Option<Record>,
}
struct Set {
    id: [u8; 16],
    participants: Vec<Participant>,
    binding: Option<Binding>,
}

#[derive(Clone, PartialEq, Eq)]
struct Immutable {
    path: PathBuf,
    identity: (u64, u64),
    length: u64,
    digest: [u8; 32],
}
#[derive(Clone, PartialEq, Eq)]
struct Binding {
    dependencies: Vec<Immutable>,
    nodes: Vec<DependencyNode>,
}
impl Binding {
    fn matches_graph(&self, graph: &PinnedParentGraph) -> bool {
        self.nodes == graph.nodes
            && self.dependencies.len() == graph.dependencies.len()
            && self
                .dependencies
                .iter()
                .zip(&graph.dependencies)
                .all(|(binding, pin)| {
                    binding.path == pin.path
                        && binding.identity == pin.identity
                        && binding.length == pin.length
                        && binding.digest == pin.digest
                })
    }
    fn from_graph(graph: &PinnedParentGraph) -> Self {
        Self {
            dependencies: graph
                .dependencies
                .iter()
                .map(|pin| Immutable {
                    path: pin.path.clone(),
                    identity: pin.identity,
                    length: pin.length,
                    digest: pin.digest,
                })
                .collect(),
            nodes: graph.nodes.clone(),
        }
    }
    fn validate(&self, participants: &[Participant]) -> io::Result<()> {
        if self.dependencies.is_empty()
            || self.nodes.is_empty()
            || self.nodes.len() > 64
            || participants.len() + self.dependencies.len() > MAX_FILES
        {
            return Err(invalid());
        }
        let mut physical = 0u64;
        for participant in participants {
            physical = physical
                .checked_add(
                    participant
                        .record
                        .as_ref()
                        .map_or(participant.length, |record| {
                            record.final_length.max(participant.length)
                        }),
                )
                .ok_or_else(invalid)?;
        }
        for (index, dependency) in self.dependencies.iter().enumerate() {
            let bytes = dependency.path.as_os_str().as_bytes();
            if !dependency.path.is_absolute()
                || bytes.is_empty()
                || bytes.len() > 4096
                || bytes.contains(&0)
                || participants
                    .iter()
                    .any(|p| p.path == dependency.path || p.identity == dependency.identity)
                || self.dependencies[..index]
                    .iter()
                    .any(|p| p.path == dependency.path || p.identity == dependency.identity)
            {
                return Err(invalid());
            }
            physical = physical
                .checked_add(dependency.length)
                .ok_or_else(invalid)?;
        }
        if physical > 33 * 1024 * 1024 * 1024 {
            return Err(invalid());
        }
        let mut used = vec![false; self.dependencies.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if node.descriptor >= used.len()
                || used[node.descriptor]
                || node.extents.is_empty()
                || node.extents.len() > MAX_FILES
                || node.capacity == 0
                || node.capacity % 512 != 0
            {
                return Err(invalid());
            }
            used[node.descriptor] = true;
            match node.parent {
                Some(parent)
                    if parent == index + 1
                        && parent < self.nodes.len()
                        && node.parent_cid != u32::MAX
                        && self.nodes[parent].cid == node.parent_cid
                        && self.nodes[parent].capacity == node.capacity => {}
                None if index + 1 == self.nodes.len() && node.parent_cid == u32::MAX => {}
                _ => return Err(invalid()),
            }
            let mut end = 0u64;
            for extent in &node.extents {
                if extent.file >= used.len()
                    || extent.start != end
                    || extent.length == 0
                    || extent.length % 512 != 0
                {
                    return Err(invalid());
                }
                let hosted = extent.file == node.descriptor
                    && node.extents.len() == 1
                    && extent.kind == DependencyExtentKind::HostedSparse;
                if used[extent.file] && !hosted {
                    return Err(invalid());
                }
                used[extent.file] = true;
                match extent.kind {
                    DependencyExtentKind::HostedSparse if extent.offset == 0 => {}
                    DependencyExtentKind::Flat
                        if extent.offset % 512 == 0
                            && extent
                                .offset
                                .checked_add(extent.length)
                                .is_some_and(|n| n <= self.dependencies[extent.file].length) => {}
                    _ => return Err(invalid()),
                }
                end = end.checked_add(extent.length).ok_or_else(invalid)?;
            }
            if end != node.capacity {
                return Err(invalid());
            }
        }
        if used.iter().any(|value| !value) {
            return Err(invalid());
        }
        Ok(())
    }
    fn encode(&self, out: &mut Vec<u8>) -> io::Result<()> {
        push_u64(out, self.dependencies.len() as u64);
        for pin in &self.dependencies {
            let path = pin.path.as_os_str().as_bytes();
            push_u64(out, path.len() as u64);
            out.extend_from_slice(path);
            push_u64(out, pin.identity.0);
            push_u64(out, pin.identity.1);
            push_u64(out, pin.length);
            out.extend_from_slice(&pin.digest);
        }
        push_u64(out, self.nodes.len() as u64);
        for node in &self.nodes {
            for value in [
                node.descriptor as u64,
                u64::from(node.cid),
                u64::from(node.parent_cid),
                node.parent.map_or(u64::MAX, |n| n as u64),
                node.capacity,
                node.extents.len() as u64,
            ] {
                push_u64(out, value);
            }
            for extent in &node.extents {
                let kind = match extent.kind {
                    DependencyExtentKind::HostedSparse => 0,
                    DependencyExtentKind::Flat => 1,
                };
                for value in [
                    extent.file as u64,
                    kind,
                    extent.start,
                    extent.length,
                    extent.offset,
                ] {
                    push_u64(out, value);
                }
            }
            if out.len() + 32 > LIMIT {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn decode(cursor: &mut Cursor<'_>) -> io::Result<Self> {
        let count = cursor.number()?;
        if count == 0 || count > MAX_FILES as u64 {
            return Err(invalid());
        }
        let mut dependencies = Vec::new();
        for _ in 0..count {
            let size = cursor.number()?;
            if size == 0 || size > 4096 {
                return Err(invalid());
            }
            dependencies.push(Immutable {
                path: PathBuf::from(std::ffi::OsString::from_vec(
                    cursor.take(size as usize)?.to_vec(),
                )),
                identity: (cursor.number()?, cursor.number()?),
                length: cursor.number()?,
                digest: cursor.take(32)?.try_into().map_err(|_| invalid())?,
            });
        }
        let count = cursor.number()?;
        if count == 0 || count > 64 {
            return Err(invalid());
        }
        let mut nodes = Vec::new();
        for _ in 0..count {
            let descriptor = usize::try_from(cursor.number()?).map_err(|_| invalid())?;
            let cid = u32::try_from(cursor.number()?).map_err(|_| invalid())?;
            let parent_cid = u32::try_from(cursor.number()?).map_err(|_| invalid())?;
            let parent = match cursor.number()? {
                u64::MAX => None,
                n => Some(usize::try_from(n).map_err(|_| invalid())?),
            };
            let capacity = cursor.number()?;
            let count = cursor.number()?;
            if count == 0 || count > MAX_FILES as u64 {
                return Err(invalid());
            }
            let mut extents = Vec::new();
            for _ in 0..count {
                let file = usize::try_from(cursor.number()?).map_err(|_| invalid())?;
                let kind = match cursor.number()? {
                    0 => DependencyExtentKind::HostedSparse,
                    1 => DependencyExtentKind::Flat,
                    _ => return Err(invalid()),
                };
                extents.push(DependencyExtent {
                    file,
                    kind,
                    start: cursor.number()?,
                    length: cursor.number()?,
                    offset: cursor.number()?,
                });
            }
            nodes.push(DependencyNode {
                descriptor,
                cid,
                parent_cid,
                parent,
                capacity,
                extents,
            });
        }
        Ok(Self {
            dependencies,
            nodes,
        })
    }
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, size: usize) -> io::Result<&'a [u8]> {
        let end = self.at.checked_add(size).ok_or_else(invalid)?;
        let out = self.bytes.get(self.at..end).ok_or_else(invalid)?;
        self.at = end;
        Ok(out)
    }
    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(|_| invalid())?,
        ))
    }
}
impl Set {
    fn encode(&self) -> io::Result<Vec<u8>> {
        if self.participants.is_empty() || self.participants.len() > MAX_FILES {
            return Err(invalid());
        }
        if let Some(binding) = &self.binding {
            binding.validate(&self.participants)?;
        }
        let mut out = if self.binding.is_some() {
            BACKED_MAGIC
        } else {
            MAGIC
        }
        .to_vec();
        out.extend_from_slice(&self.id);
        push_u64(&mut out, self.participants.len() as u64);
        let mut patches = 0;
        let mut physical = 0u64;
        for participant in &self.participants {
            physical = physical
                .checked_add(
                    participant
                        .record
                        .as_ref()
                        .map_or(participant.length, |record| {
                            record.final_length.max(participant.length)
                        }),
                )
                .ok_or_else(invalid)?;
            if physical > 33 * 1024 * 1024 * 1024 {
                return Err(invalid());
            }
            let path = participant.path.as_os_str().as_bytes();
            if !participant.path.is_absolute()
                || path.is_empty()
                || path.len() > 4096
                || path.contains(&0)
            {
                return Err(invalid());
            }
            push_u64(&mut out, path.len() as u64);
            out.extend_from_slice(path);
            push_u64(&mut out, participant.identity.0);
            push_u64(&mut out, participant.identity.1);
            push_u64(&mut out, participant.length);
            out.extend_from_slice(&participant.digest);
            let bytes = match &participant.record {
                Some(record) => {
                    patches += record.patches.len();
                    if record.original_length != participant.length
                        || record.original_digest != participant.digest
                        || record.final_length < record.original_length
                    {
                        return Err(invalid());
                    }
                    record.encode()?
                }
                None => Vec::new(),
            };
            if patches > 16 {
                return Err(invalid());
            }
            push_u64(&mut out, bytes.len() as u64);
            out.extend_from_slice(&bytes);
            if out.len() + 32 > LIMIT {
                return Err(invalid());
            }
        }
        if let Some(binding) = &self.binding {
            binding.encode(&mut out)?;
        }
        if out.len() + 32 > LIMIT {
            return Err(invalid());
        }
        let checksum = Sha256::digest(&out);
        out.extend_from_slice(&checksum);
        Ok(out)
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 64 || bytes.len() > LIMIT {
            return Err(invalid());
        }
        let body = &bytes[..bytes.len() - 32];
        if Sha256::digest(body)[..] != bytes[bytes.len() - 32..] {
            return Err(invalid());
        }
        let mut cursor = Cursor { bytes: body, at: 0 };
        let backed = match cursor.take(8)? {
            magic if magic == MAGIC => false,
            magic if magic == BACKED_MAGIC => true,
            _ => return Err(invalid()),
        };
        let id = cursor.take(16)?.try_into().map_err(|_| invalid())?;
        let count = cursor.number()?;
        if count == 0 || count > MAX_FILES as u64 {
            return Err(invalid());
        }
        let mut participants = Vec::new();
        for _ in 0..count {
            let size = cursor.number()?;
            if size == 0 || size > 4096 {
                return Err(invalid());
            }
            let path = PathBuf::from(std::ffi::OsString::from_vec(
                cursor.take(size as usize)?.to_vec(),
            ));
            let identity = (cursor.number()?, cursor.number()?);
            let length = cursor.number()?;
            let digest = cursor.take(32)?.try_into().map_err(|_| invalid())?;
            let size = cursor.number()?;
            if size > LIMIT as u64 {
                return Err(invalid());
            }
            let record = if size == 0 {
                None
            } else {
                Some(Record::decode(cursor.take(size as usize)?)?)
            };
            if participants
                .iter()
                .any(|p: &Participant| p.path == path || p.identity == identity)
            {
                return Err(invalid());
            }
            participants.push(Participant {
                path,
                identity,
                length,
                digest,
                record,
            });
        }
        let binding = if backed {
            Some(Binding::decode(&mut cursor)?)
        } else {
            None
        };
        if cursor.at != body.len() {
            return Err(invalid());
        }
        let set = Self {
            id,
            participants,
            binding,
        };
        set.encode()?;
        Ok(set)
    }
    fn marker(&self, descriptor: &Path) -> io::Result<Vec<u8>> {
        let path = descriptor.as_os_str().as_bytes();
        if path.len() > 4096 {
            return Err(invalid());
        }
        let mut bytes = MARKER.to_vec();
        bytes.extend_from_slice(&self.id);
        push_u64(&mut bytes, path.len() as u64);
        bytes.extend_from_slice(path);
        let digest = Sha256::digest(&bytes);
        bytes.extend_from_slice(&digest);
        Ok(bytes)
    }
}

type Validator<'a> = &'a dyn Fn(&[Arc<dyn ReadAt>]) -> io::Result<()>;
fn validate(
    set: &Set,
    files: &[File],
    graph: Option<&PinnedParentGraph>,
    validator: Validator<'_>,
) -> io::Result<()> {
    if files.len() != set.participants.len() {
        return Err(invalid());
    }
    match (&set.binding, graph) {
        (None, None) => {}
        (Some(binding), Some(graph)) if binding.matches_graph(graph) => {
            binding.validate(&set.participants)?;
            graph.verify()?;
            let _digest_scratch = graph.budget.cache(65536)?;
            for pin in &graph.dependencies {
                graph.budget.work(pin.length.div_ceil(65536))?;
                if transaction::digest_reader(pin)? != pin.digest {
                    return Err(invalid());
                }
                pin.verify()?;
            }
        }
        _ => return Err(invalid()),
    }
    let mut originals = Vec::new();
    let mut proposed = Vec::new();
    for (file, participant) in files.iter().zip(&set.participants) {
        let _digest_scratch = file.budget.cache(65536)?;
        if file.path != participant.path || file.verify_path()? != participant.identity {
            return Err(invalid());
        }
        let source: Arc<dyn ReadAt> = Arc::new(Source {
            raw: file.raw.clone(),
            length: file.raw.len(),
        });
        // Digest reconstruction uses fixed 64 KiB chunks; charge scans to the
        // same cumulative budget used by both whole-set parser states.
        file.budget.work(participant.length.div_ceil(65536))?;
        if let Some(record) = &participant.record {
            record.validate_original(&*source)?;
            originals.push(transaction::shadow(source.clone(), record.clone(), false));
            proposed.push(transaction::shadow(source, record.clone(), true));
        } else {
            if source.len() != participant.length
                || transaction::digest_reader(&*source)? != participant.digest
            {
                return Err(invalid());
            }
            originals.push(source.clone());
            proposed.push(source);
        }
    }
    validator(&originals)?;
    validator(&proposed)
}

fn read_sidecar(path: &Path) -> io::Result<Option<Vec<u8>>> {
    use io::Read;
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x20000 | 0x800)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    (&mut file).take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid());
    }
    Ok(Some(bytes))
}
fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use io::Write;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::hard_link(&temporary, path)?;
    sync_parent(path)?;
    std::fs::remove_file(temporary)?;
    sync_parent(path)
}
fn interrupt(stage: usize, cut: Option<usize>) -> io::Result<()> {
    if cut == Some(stage) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "injected transaction set interruption",
        ))
    } else {
        Ok(())
    }
}

fn replay(
    set: &Set,
    files: &[File],
    cut: Option<usize>,
    graph: Option<&PinnedParentGraph>,
    validator: Validator<'_>,
) -> io::Result<()> {
    validate(set, files, graph, validator)?;
    let marker = set.marker(&files[0].path)?;
    // Validate every marker before publishing any missing marker or image patch.
    for file in &files[1..] {
        if let Some(bytes) = read_sidecar(&transaction::sidecar(&file.path))?
            && bytes != marker
        {
            return Err(invalid());
        }
    }
    let mut stage = 1;
    for file in &files[1..] {
        let path = transaction::sidecar(&file.path);
        if read_sidecar(&path)?.is_none() {
            publish(&path, &marker)?;
        }
        interrupt(stage, cut)?;
        stage += 1;
    }
    // Descriptor CID is a separate durability barrier before extent patches.
    for (index, (file, participant)) in files.iter().zip(&set.participants).enumerate() {
        file.verify_path()?;
        if let Some(record) = &participant.record {
            file.raw.resize(record.final_length)?;
            interrupt(stage, cut)?;
            stage += 1;
            let mut patches = record.patches.iter().collect::<Vec<_>>();
            patches.sort_unstable_by_key(|patch| patch.order);
            for patch in patches {
                file.raw.write_all_at(patch.offset, &patch.new)?;
                interrupt(stage, cut)?;
                stage += 1;
            }
            file.raw.flush()?;
            interrupt(stage, cut)?;
            stage += 1;
        }
        if index == 0 {
            interrupt(stage, cut)?;
            stage += 1;
        }
    }
    for file in &files[1..] {
        file.verify_path()?;
        let path = transaction::sidecar(&file.path);
        if read_sidecar(&path)?.as_deref() != Some(&marker) {
            return Err(invalid());
        }
        std::fs::remove_file(&path)?;
        sync_parent(&path)?;
        interrupt(stage, cut)?;
        stage += 1;
    }
    files[0].verify_path()?;
    let path = transaction::sidecar(&files[0].path);
    if read_sidecar(&path)?.as_deref() != Some(set.encode()?.as_slice()) {
        return Err(invalid());
    }
    std::fs::remove_file(&path)?;
    sync_parent(&path)?;
    interrupt(stage, cut)
}

pub(super) fn commit(
    files: &[File],
    records: Vec<Option<Record>>,
    cut: Option<usize>,
    validator: Validator<'_>,
) -> io::Result<()> {
    commit_bound(files, records, None, cut, validator)
}
pub(super) fn commit_bound(
    files: &[File],
    records: Vec<Option<Record>>,
    graph: Option<&PinnedParentGraph>,
    cut: Option<usize>,
    validator: Validator<'_>,
) -> io::Result<()> {
    if files.len() != records.len() || files.is_empty() || files.len() > MAX_FILES {
        return Err(invalid());
    }
    // Charge the owned snapshot before copying retained parent paths/topology.
    let _binding_cache = if let Some(graph) = graph {
        let mut retained = 0u64;
        for pin in &graph.dependencies {
            retained = retained
                .checked_add(pin.path.as_os_str().as_bytes().len() as u64 + 128)
                .ok_or_else(invalid)?;
        }
        for node in &graph.nodes {
            retained = retained
                .checked_add(128 + (node.extents.len() as u64) * 64)
                .ok_or_else(invalid)?;
        }
        graph.budget.metadata(retained)?;
        Some(graph.budget.cache(retained)?)
    } else {
        None
    };
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(|error| io::Error::other(error.to_string()))?;
    let mut participants = Vec::new();
    for (file, record) in files.iter().zip(records) {
        let identity = file.verify_path()?;
        if participants
            .iter()
            .any(|p: &Participant| p.identity == identity || p.path == file.path)
        {
            return Err(invalid());
        }
        if transaction::pending(&file.path)? {
            return Err(invalid());
        }
        sync_parent(&file.path)?;
        let source = Source {
            raw: file.raw.clone(),
            length: file.raw.len(),
        };
        file.budget.work(source.len().div_ceil(65536))?;
        let _digest_scratch = file.budget.cache(65536)?;
        let digest = transaction::digest_reader(&source)?;
        participants.push(Participant {
            path: file.path.clone(),
            identity,
            length: source.len(),
            digest,
            record,
        });
    }
    let set = Set {
        id,
        participants,
        binding: graph.map(Binding::from_graph),
    };
    let bytes = set.encode()?;
    files[0].budget.metadata(bytes.len() as u64)?;
    let _journal_cache = files[0].budget.cache((bytes.len() as u64) * 4)?;
    validate(&set, files, graph, validator)?;
    publish(&transaction::sidecar(&files[0].path), &bytes)?;
    interrupt(0, cut)?;
    replay(&set, files, cut, graph, validator)
}
pub(super) fn recover(files: &[File], validator: Validator<'_>) -> io::Result<()> {
    recover_bound(files, None, validator)
}
pub(super) fn recover_bound(
    files: &[File],
    graph: Option<&PinnedParentGraph>,
    validator: Validator<'_>,
) -> io::Result<()> {
    let first = files.first().ok_or_else(invalid)?;
    if let Some(bytes) = read_sidecar(&transaction::sidecar(&first.path))? {
        first.budget.metadata(bytes.len() as u64)?;
        let _journal_cache = first.budget.cache((bytes.len() as u64) * 4)?;
        let set = Set::decode(&bytes)?;
        replay(&set, files, None, graph, validator)?;
    } else {
        for file in &files[1..] {
            if transaction::pending(&file.path)? {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;
    #[allow(dead_code)]
    mod frozen_legacy {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vmdk/legacy-set-codec.rs"
        ));
    }
    fn participant(path: &str, ino: u64) -> Participant {
        Participant {
            path: PathBuf::from(path),
            identity: (1, ino),
            length: 512,
            digest: [3; 32],
            record: None,
        }
    }
    fn backed_fixture() -> (Vec<u8>, Vec<u8>) {
        let set = Set {
            id: [7; 16],
            participants: vec![participant("/child", 1)],
            binding: None,
        };
        let legacy = set.encode().unwrap();
        let mut body = legacy[..legacy.len() - 32].to_vec();
        body[..8].copy_from_slice(b"VDTXSET2");
        push_u64(&mut body, 1); // Immutable physical dependencies.
        push_u64(&mut body, 7);
        body.extend_from_slice(b"/parent");
        push_u64(&mut body, 1);
        push_u64(&mut body, 2);
        push_u64(&mut body, 65536);
        body.extend_from_slice(&[4; 32]);
        push_u64(&mut body, 1); // Direct parent, standalone hosted sparse.
        push_u64(&mut body, 0); // Descriptor dependency index.
        push_u64(&mut body, 0x31415926);
        push_u64(&mut body, 0xffffffff);
        push_u64(&mut body, u64::MAX); // No ancestor edge.
        push_u64(&mut body, 65536);
        push_u64(&mut body, 1); // Extents.
        push_u64(&mut body, 0); // Hosted descriptor and extent share a file.
        push_u64(&mut body, 0); // Hosted sparse kind.
        push_u64(&mut body, 0);
        push_u64(&mut body, 65536);
        push_u64(&mut body, 0);
        let checksum = Sha256::digest(&body);
        body.extend_from_slice(&checksum);
        (legacy, body)
    }
    #[test]
    fn backed_codec_accepts_explicit_parent_manifest_without_changing_legacy_bytes() {
        let (legacy, body) = backed_fixture();
        let decoded = Set::decode(&body).expect("explicit immutable dependency codec");
        assert_eq!(decoded.encode().unwrap(), body);
        assert_eq!(Set::decode(&legacy).unwrap().encode().unwrap(), legacy);
        assert!(frozen_legacy::accepts(&legacy));
        assert!(!frozen_legacy::accepts(&body));
    }
    #[test]
    fn backed_codec_rejects_checksum_valid_hostile_roles_aliases_and_topology() {
        let (legacy, body) = backed_fixture();
        let valid = Set::decode(&body).unwrap().binding.unwrap();
        let hostile = |binding: Binding| {
            // Bypass the encoder's semantic validation to model an attacker
            // who supplies a correctly checksummed but structurally invalid journal.
            let mut bytes = legacy[..legacy.len() - 32].to_vec();
            bytes[..8].copy_from_slice(BACKED_MAGIC);
            binding.encode(&mut bytes).unwrap();
            let checksum = Sha256::digest(&bytes);
            bytes.extend_from_slice(&checksum);
            assert!(Set::decode(&bytes).is_err());
        };
        let mut bad = valid.clone();
        bad.dependencies.clear();
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies[0].path = PathBuf::from("/child");
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies[0].identity = (1, 1);
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies[0].path = PathBuf::from("relative-parent");
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies[0].path =
            PathBuf::from(std::ffi::OsString::from_vec(b"/parent\0tail".to_vec()));
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies[0].length = 33 * 1024 * 1024 * 1024;
        hostile(bad);
        let mut bad = valid.clone();
        bad.dependencies.push(bad.dependencies[0].clone());
        hostile(bad);
        let mut bad = valid.clone();
        let mut unused = bad.dependencies[0].clone();
        unused.path = PathBuf::from("/unused");
        unused.identity = (1, 3);
        bad.dependencies.push(unused);
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes.clear();
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].descriptor = 1;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].parent = Some(0);
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].parent_cid = 0x31415926;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].capacity += 512;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].extents[0].file = 1;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].extents[0].start = 512;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].extents[0].offset = 512;
        hostile(bad);
        let mut bad = valid.clone();
        bad.nodes[0].extents[0].kind = DependencyExtentKind::Flat;
        hostile(bad); // A descriptor cannot also be a flat extent.
        let mut bad = valid.clone();
        let repeated = bad.nodes[0].extents[0].clone();
        bad.nodes[0].extents.push(repeated);
        hostile(bad);
        for end in [0, 8, 32, body.len() - 33, body.len() - 1] {
            assert!(Set::decode(&body[..end]).is_err());
        }
        let mut unknown_kind = body.clone();
        let kind = unknown_kind.len() - 64;
        unknown_kind[kind..kind + 8].copy_from_slice(&2u64.to_be_bytes());
        let end = unknown_kind.len() - 32;
        let checksum = Sha256::digest(&unknown_kind[..end]);
        unknown_kind[end..].copy_from_slice(&checksum);
        assert!(Set::decode(&unknown_kind).is_err());
    }
    #[test]
    fn backed_codec_preserves_ancestor_edges_and_distinct_flat_physical_roles() {
        let (_, bytes) = backed_fixture();
        let mut set = Set::decode(&bytes).unwrap();
        let binding = set.binding.as_mut().unwrap();
        let mut ancestor = binding.dependencies[0].clone();
        ancestor.path = PathBuf::from("/ancestor");
        ancestor.identity = (1, 3);
        binding.dependencies.push(ancestor);
        let mut node = binding.nodes[0].clone();
        node.cid = 0x27182818;
        node.descriptor = 1;
        node.extents[0].file = 1;
        binding.nodes[0].parent = Some(1);
        binding.nodes[0].parent_cid = node.cid;
        binding.nodes.push(node);
        let bytes = set.encode().unwrap();
        assert_eq!(Set::decode(&bytes).unwrap().encode().unwrap(), bytes);
        set.binding.as_mut().unwrap().nodes[1].cid ^= 1;
        assert!(set.encode().is_err());

        let (_, bytes) = backed_fixture();
        let mut flat = Set::decode(&bytes).unwrap();
        let binding = flat.binding.as_mut().unwrap();
        binding.dependencies[0].length = 512;
        let mut payload = binding.dependencies[0].clone();
        payload.path = PathBuf::from("/flat-payload");
        payload.identity = (1, 3);
        payload.length = 66048;
        binding.dependencies.push(payload);
        binding.nodes[0].extents[0].file = 1;
        binding.nodes[0].extents[0].kind = DependencyExtentKind::Flat;
        binding.nodes[0].extents[0].offset = 512;
        let bytes = flat.encode().unwrap();
        assert_eq!(Set::decode(&bytes).unwrap().encode().unwrap(), bytes);
        flat.binding.as_mut().unwrap().dependencies[1].length -= 1;
        assert!(flat.encode().is_err());
    }
    #[test]
    fn changed_parent_recovery_refuses_before_child_or_sidecar_mutation() {
        let _boundary = crate::test_sync::writer_test();
        for change in 0..5 {
            let directory = tempfile::tempdir().unwrap();
            let parent = directory.path().join("parent.vmdk");
            let extent = directory.path().join("parent-s1.vmdk");
            drop(crate::VmdkWriter::create_sparse(&extent, 65536).unwrap());
            let mut bytes = std::fs::read(&extent).unwrap();
            let offset = u64::from_le_bytes(bytes[28..36].try_into().unwrap()) as usize * 512;
            let length = u64::from_le_bytes(bytes[36..44].try_into().unwrap()) as usize * 512;
            bytes[offset..offset + length].fill(0);
            std::fs::write(&extent, bytes).unwrap();
            std::fs::write(&parent, "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"parent-s1.vmdk\"\n").unwrap();
            let child = directory.path().join("child.vmdk");
            std::fs::write(&child, "version=1\nCID=87654321\nparentCID=12345678\nparentFileNameHint=\"parent.vmdk\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"child-s1.vmdk\"\n").unwrap();
            let budget = crate::ReadBudget::new(crate::ParserLimits::default()).unwrap();
            let raw = Arc::new(RawWriter::open(&child).unwrap());
            let identities = [crate::RawDisk::open(&child).unwrap().identity().unwrap()];
            let authorized = [parent.clone(), extent.clone()];
            let source: Arc<dyn ReadAt> = Arc::new(Source {
                raw: raw.clone(),
                length: raw.len(),
            });
            let graph = crate::Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized,
                &budget,
                &identities,
                1,
                raw.len(),
            )
            .unwrap()
            .unwrap();
            let original = std::fs::read(&child).unwrap();
            let record = Record {
                original_length: raw.len(),
                final_length: raw.len(),
                original_digest: transaction::digest_reader(&*source).unwrap(),
                patches: vec![transaction::Patch {
                    order: 0,
                    offset: 14,
                    old: original[14..22].to_vec(),
                    new: b"abcdef01".to_vec(),
                }],
            };
            let files = [File {
                path: child.clone(),
                raw,
                budget: budget.clone(),
            }];
            assert_eq!(
                commit_bound(&files, vec![Some(record)], Some(&graph), Some(0), &|_| Ok(
                    ()
                ))
                .unwrap_err()
                .kind(),
                io::ErrorKind::Interrupted
            );
            let pending_bytes = std::fs::read(transaction::sidecar(&child)).unwrap();
            assert!(recover(&files, &|_| Ok(())).is_err());
            assert_eq!(std::fs::read(&child).unwrap(), original);
            assert_eq!(
                std::fs::read(transaction::sidecar(&child)).unwrap(),
                pending_bytes
            );
            // Last case retains the original graph, so equality alone passes
            // and the live digest rescan must detect noncooperating modification.
            let retained_graph = if change == 4 {
                Some(graph)
            } else {
                drop(graph);
                None
            };
            match change {
                0 | 4 => {
                    // Keep parent CID and file identity/length; change payload padding.
                    let mut bytes = std::fs::read(&extent).unwrap();
                    *bytes.last_mut().unwrap() ^= 1;
                    std::fs::write(&extent, bytes).unwrap();
                }
                1 => {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&extent)
                        .unwrap()
                        .write_all(&[0; 512])
                        .unwrap();
                }
                2 => {
                    let bytes = std::fs::read(&extent).unwrap();
                    std::fs::rename(&extent, directory.path().join("old-extent")).unwrap();
                    std::fs::write(&extent, bytes).unwrap();
                }
                3 => {
                    std::fs::write(
                        &parent,
                        std::fs::read(&parent)
                            .unwrap()
                            .into_iter()
                            .chain(b"# same CID\n".iter().copied())
                            .collect::<Vec<_>>(),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let sidecar = transaction::sidecar(&child);
            let before = std::fs::read(&sidecar).unwrap();
            let parent_before = std::fs::read(&parent).unwrap();
            let extent_before = std::fs::read(&extent).unwrap();
            let names = || {
                let mut paths = std::fs::read_dir(directory.path())
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>();
                paths.sort();
                paths
            };
            let names_before = names();
            let graph = retained_graph.unwrap_or_else(|| {
                crate::Vmdk::resolve_pinned_parent(
                    source.clone(),
                    &child,
                    &authorized,
                    &budget,
                    &identities,
                    1,
                    files[0].raw.len(),
                )
                .unwrap()
                .unwrap()
            });
            assert!(
                recover_bound(&files, Some(&graph), &|_| Ok(())).is_err(),
                "change {change}"
            );
            assert_eq!(std::fs::read(&child).unwrap(), original);
            assert_eq!(std::fs::read(&sidecar).unwrap(), before);
            assert_eq!(std::fs::read(&parent).unwrap(), parent_before);
            assert_eq!(std::fs::read(&extent).unwrap(), extent_before);
            assert_eq!(names(), names_before);
            assert!(!transaction::pending(&extent).unwrap());
            assert!(!transaction::pending(&parent).unwrap());
        }
    }
    #[test]
    fn codec_rejects_truncation_checksum_aliases_and_aggregate_limits() {
        let mut set = Set {
            id: [7; 16],
            participants: vec![participant("/descriptor", 1), participant("/extent", 2)],
            binding: None,
        };
        let bytes = set.encode().unwrap();
        assert_eq!(Set::decode(&bytes).unwrap().encode().unwrap(), bytes);
        for cut in [0, 7, 31, bytes.len() - 1] {
            assert!(Set::decode(&bytes[..cut]).is_err());
        }
        let mut corrupt = bytes.clone();
        corrupt[24] ^= 1;
        assert!(Set::decode(&corrupt).is_err());
        set.participants[1].path = set.participants[0].path.clone();
        assert!(Set::decode(&set.encode().unwrap()).is_err());
        set.participants[1].path = PathBuf::from("/extent");
        set.participants[1].identity = set.participants[0].identity;
        assert!(Set::decode(&set.encode().unwrap()).is_err());
        set.participants[1].identity = (1, 2);
        set.participants[1].length = 33 * 1024 * 1024 * 1024;
        assert!(set.encode().is_err());
        set.participants[1].length = 512;
        for item in &mut set.participants {
            item.record = Some(Record {
                original_length: 512,
                final_length: 512,
                original_digest: [3; 32],
                patches: (0..9)
                    .map(|order| transaction::Patch {
                        order,
                        offset: u64::from(order) * 4,
                        old: vec![0; 4],
                        new: vec![1; 4],
                    })
                    .collect(),
            });
        }
        assert!(set.encode().is_err());
    }
    #[test]
    fn marker_binds_exact_transaction_and_coordinator_path() {
        let mut set = Set {
            id: [7; 16],
            participants: vec![participant("/descriptor", 1)],
            binding: None,
        };
        let first = set.marker(Path::new("/descriptor")).unwrap();
        set.id[0] ^= 1;
        assert_ne!(first, set.marker(Path::new("/descriptor")).unwrap());
        set.id[0] ^= 1;
        assert_ne!(first, set.marker(Path::new("/other")).unwrap());
    }
    #[test]
    fn aggregate_final_lengths_cannot_bypass_original_physical_bound() {
        let mut set = Set {
            id: [7; 16],
            participants: vec![participant("/descriptor", 1), participant("/extent", 2)],
            binding: None,
        };
        for item in &mut set.participants {
            let record = Record {
                original_length: 512,
                final_length: 17 * 1024 * 1024 * 1024,
                original_digest: item.digest,
                patches: vec![transaction::Patch {
                    order: 0,
                    offset: 512,
                    old: vec![],
                    new: vec![0; 512],
                }],
            };
            assert!(record.encode().is_ok());
            item.record = Some(record);
        }
        assert!(set.encode().is_err());
    }
    #[test]
    fn fifo_sidecar_refuses_without_waiting_for_a_writer() {
        unsafe extern "C" {
            fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("marker");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live NUL-terminated path and Linux mode_t is u32.
        assert_eq!(unsafe { mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_sidecar(&path).is_err());
        assert!(
            std::fs::symlink_metadata(path)
                .unwrap()
                .file_type()
                .is_fifo()
        );
    }
}
