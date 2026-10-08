//! Immutable physical parent graph retained by Linux split writers.
use super::*;
use std::os::unix::fs::MetadataExt;

pub(crate) struct DependencyPin {
    pub(crate) path: PathBuf,
    pub(crate) raw: Arc<crate::RawDisk>,
    pub(crate) identity: (u64, u64),
    pub(crate) length: u64,
    pub(crate) digest: [u8; 32],
}
impl DependencyPin {
    pub(crate) fn verify(&self) -> io::Result<()> {
        let opened = self.raw.pin_metadata()?;
        let named = std::fs::symlink_metadata(&self.path)?;
        if !opened.is_file()
            || !named.is_file()
            || (opened.dev(), opened.ino()) != self.identity
            || (named.dev(), named.ino()) != self.identity
            || opened.len() != self.length
            || named.len() != self.length
            || self.path.canonicalize()? != self.path
            || crate::transaction::pending(&self.path)?
        {
            return Err(invalid(
                "VMDK pinned dependency identity, length or path changed",
            ));
        }
        Ok(())
    }
}
impl ReadAt for DependencyPin {
    fn len(&self) -> u64 {
        self.length
    }
    fn context(&self) -> ReadContext {
        self.raw.context()
    }
    fn read_exact_at(&self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        self.raw.read_exact_at(offset, bytes)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DependencyExtentKind {
    HostedSparse,
    Flat,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyExtent {
    pub(crate) file: usize,
    pub(crate) kind: DependencyExtentKind,
    pub(crate) start: u64,
    pub(crate) length: u64,
    pub(crate) offset: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyNode {
    pub(crate) descriptor: usize,
    pub(crate) cid: u32,
    pub(crate) parent_cid: u32,
    pub(crate) parent: Option<usize>,
    pub(crate) capacity: u64,
    pub(crate) extents: Vec<DependencyExtent>,
}
pub(crate) struct PinnedParentGraph {
    pub(crate) reader: Arc<Vmdk>,
    pub(crate) dependencies: Vec<DependencyPin>,
    pub(crate) nodes: Vec<DependencyNode>,
    pub(crate) budget: ReadBudget,
    _cache: Vec<CacheReservation>,
}
impl PinnedParentGraph {
    pub(crate) fn verify(&self) -> io::Result<()> {
        for dependency in &self.dependencies {
            self.budget.work(1)?;
            dependency.verify()?;
        }
        Ok(())
    }
    pub(crate) fn validate_child(
        &self,
        source: Arc<dyn ReadAt>,
        path: &Path,
        child_extents: &[Arc<dyn ReadAt>],
        child_paths: &[PathBuf],
    ) -> io::Result<()> {
        if child_extents.len() != child_paths.len()
            || child_extents.len() + self.dependencies.len() + 1 > 257
        {
            return Err(invalid("invalid supplied VMDK child extent set"));
        }
        self.verify()?;
        let link = descriptor_link(source.clone(), &self.budget)?;
        if link.parent_cid
            != self
                .nodes
                .first()
                .ok_or_else(|| invalid("missing VMDK parent topology"))?
                .cid
            || sector(link.sectors)? != self.reader.len()
            || link.profile != "\"twoGbMaxExtentSparse\""
            || !link.sparse_extents
        {
            return Err(invalid(
                "VMDK child parent CID, capacity or profile mismatch",
            ));
        }
        let parent_path = path
            .parent()
            .ok_or_else(|| invalid("missing VMDK child directory"))?
            .join(
                link.hint
                    .ok_or_else(|| invalid("missing VMDK child parent hint"))?,
            )
            .canonicalize()?;
        if parent_path != self.dependencies[self.nodes[0].descriptor].path {
            return Err(invalid("VMDK child parent binding changed"));
        }
        let retained = child_paths
            .iter()
            .map(|path| path.as_os_str().as_encoded_bytes().len() as u64 + 256)
            .sum::<u64>();
        self.budget.metadata(retained)?;
        let _scratch = self.budget.cache(retained)?;
        let authorized = child_paths.iter().cloned().collect::<BTreeSet<_>>();
        let mut factory = FrozenChildren {
            paths: child_paths,
            sources: child_extents,
            used: vec![false; child_paths.len()],
        };
        let disk = Vmdk::parse_descriptor(
            self.budget.reader(source),
            path,
            &self.budget,
            &authorized,
            &mut Vec::new(),
            Some(self.reader.clone()),
            &mut factory,
        )?;
        if disk.len() != self.reader.len() || factory.used.iter().any(|used| !*used) {
            return Err(invalid(
                "VMDK child shadow does not match retained extent set",
            ));
        }
        Ok(())
    }
}
struct FrozenChildren<'a> {
    paths: &'a [PathBuf],
    sources: &'a [Arc<dyn ReadAt>],
    used: Vec<bool>,
}
impl SourceFactory for FrozenChildren<'_> {
    fn allow_pending(&self) -> bool {
        true
    }
    fn open(&mut self, path: &Path, budget: &ReadBudget) -> io::Result<OpenedSource> {
        budget.work(self.paths.len() as u64)?;
        let index = self
            .paths
            .iter()
            .position(|known| known == path)
            .ok_or_else(|| invalid("unknown VMDK child shadow source"))?;
        if self.used[index] || self.used.iter().filter(|used| **used).count() != index {
            return Err(invalid("repeated or reordered VMDK child shadow source"));
        }
        self.used[index] = true;
        let mut context = self.sources[index].context();
        context.container = None;
        Ok(OpenedSource {
            source: crate::contextual_reader(self.sources[index].clone(), context),
            identity: None,
        })
    }
}
fn descriptor_link(source: Arc<dyn ReadAt>, budget: &ReadBudget) -> io::Result<Link> {
    if source.is_empty() || source.len() > 65536 {
        return Err(invalid("bounded VMDK split descriptor required"));
    }
    budget.metadata(source.len())?;
    let _scratch = budget.cache(source.len())?;
    let mut bytes = vec![0; source.len() as usize];
    budget.work(1)?;
    source.read_exact_at(0, &mut bytes)?;
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    if bytes[end..].iter().any(|byte| *byte != 0) {
        return Err(invalid("nonzero VMDK descriptor padding"));
    }
    Link::parse(
        std::str::from_utf8(&bytes[..end])
            .map_err(|_| invalid("invalid VMDK descriptor encoding"))?,
        budget,
    )
}
struct Discovery<'a> {
    budget: ReadBudget,
    child_identities: &'a [same_file::Handle],
    child_count: usize,
    physical: u64,
    dependencies: Vec<DependencyPin>,
    nodes: Vec<DependencyNode>,
    node_paths: Vec<PathBuf>,
    cache: Vec<CacheReservation>,
}
impl Discovery<'_> {
    fn file_index(&self, path: &Path) -> io::Result<usize> {
        self.dependencies
            .iter()
            .position(|pin| pin.path == path)
            .ok_or_else(|| invalid("unbound VMDK physical graph file"))
    }
    fn node_index(&self, path: &Path) -> io::Result<usize> {
        self.node_paths
            .iter()
            .position(|known| known == path)
            .ok_or_else(|| invalid("unbound VMDK graph node"))
    }
}
impl SourceFactory for Discovery<'_> {
    fn open(&mut self, path: &Path, budget: &ReadBudget) -> io::Result<OpenedSource> {
        if self.child_count + self.dependencies.len() >= 257
            || !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().is_empty()
            || path.as_os_str().as_encoded_bytes().len() > 4096
        {
            return Err(invalid("VMDK physical graph bound exceeded"));
        }
        let retained = 2 * path.as_os_str().as_encoded_bytes().len() as u64 + 512;
        budget.metadata(retained)?;
        let cache = budget.cache(retained)?;
        let raw = Arc::new(crate::RawDisk::open_shared_locked(path)?);
        let metadata = raw.pin_metadata()?;
        let identity = (metadata.dev(), metadata.ino());
        let handle = raw.identity()?;
        budget.work(self.child_identities.len() as u64 + self.dependencies.len() as u64)?;
        if self.child_identities.contains(&handle)
            || self.dependencies.iter().any(|pin| pin.identity == identity)
        {
            return Err(invalid("VMDK dependency aliases child or ancestor"));
        }
        let physical = self
            .physical
            .checked_add(raw.len())
            .ok_or_else(|| invalid("VMDK graph physical size overflow"))?;
        if physical > 33 * 1024 * 1024 * 1024 {
            return Err(invalid("VMDK graph physical size exceeds bound"));
        }
        budget.work(raw.len().div_ceil(65536))?;
        let digest = {
            let _scratch = budget.cache(65536)?;
            crate::transaction::digest_reader(&*raw)?
        };
        let pin = DependencyPin {
            path: path.to_path_buf(),
            length: raw.len(),
            raw: raw.clone(),
            identity,
            digest,
        };
        pin.verify()?;
        self.physical = physical;
        self.dependencies.push(pin);
        self.cache.push(cache);
        Ok(OpenedSource {
            source: raw,
            identity: Some(handle),
        })
    }
    fn node(&mut self, path: &Path, link: &Link, hosted: bool) -> io::Result<()> {
        if self.nodes.len() >= 64 || link.extent_count > 256 {
            return Err(invalid("VMDK topology bound exceeded"));
        }
        let retained =
            path.as_os_str().as_encoded_bytes().len() as u64 + 256 + link.extent_count * 128;
        self.budget.metadata(retained)?;
        let cache = self.budget.cache(retained)?;
        let descriptor = self.file_index(path)?;
        let capacity = sector(link.sectors)?;
        let extents = if hosted {
            vec![DependencyExtent {
                file: descriptor,
                kind: DependencyExtentKind::HostedSparse,
                start: 0,
                length: capacity,
                offset: 0,
            }]
        } else {
            Vec::new()
        };
        self.nodes.push(DependencyNode {
            descriptor,
            cid: link.cid,
            parent_cid: link.parent_cid,
            parent: None,
            capacity,
            extents,
        });
        self.node_paths.push(path.to_path_buf());
        self.cache.push(cache);
        Ok(())
    }
    fn parent(&mut self, child: &Path, parent: &Path) -> io::Result<()> {
        let parent = self.node_index(parent)?;
        let child = self.node_index(child)?;
        if parent <= child {
            return Err(invalid("VMDK parent graph edge is not forward"));
        }
        self.nodes[child].parent = Some(parent);
        Ok(())
    }
    fn extent(
        &mut self,
        node: &Path,
        path: &Path,
        sparse: bool,
        start: u64,
        length: u64,
        offset: u64,
    ) -> io::Result<()> {
        let file = self.file_index(path)?;
        let node = self.node_index(node)?;
        self.nodes[node].extents.push(DependencyExtent {
            file,
            kind: if sparse {
                DependencyExtentKind::HostedSparse
            } else {
                DependencyExtentKind::Flat
            },
            start,
            length,
            offset,
        });
        Ok(())
    }
}
impl Vmdk {
    pub(crate) fn resolve_pinned_parent(
        source: Arc<dyn ReadAt>,
        path: &Path,
        authorized_paths: &[PathBuf],
        budget: &ReadBudget,
        child_identities: &[same_file::Handle],
        child_count: usize,
        child_physical: u64,
    ) -> io::Result<Option<PinnedParentGraph>> {
        let link = descriptor_link(source, budget)?;
        if link.profile != "\"twoGbMaxExtentSparse\"" || !link.sparse_extents {
            return Err(unsupported(
                "pinned writer graph requires split hosted sparse child",
            ));
        }
        if link.parent_cid == u32::MAX {
            if link.hint.is_some() {
                return Err(invalid("standalone VMDK has parent hint"));
            }
            return Ok(None);
        }
        if child_count != child_identities.len()
            || child_count >= 257
            || child_physical > 33 * 1024 * 1024 * 1024
            || authorized_paths.len() > 257
        {
            return Err(invalid("VMDK writer graph count or size bound exceeded"));
        }
        let parent_path = path
            .parent()
            .ok_or_else(|| invalid("missing VMDK child directory"))?
            .join(
                link.hint
                    .ok_or_else(|| invalid("missing VMDK parent hint"))?,
            )
            .canonicalize()?;
        let authorized = authorize(authorized_paths, budget)?;
        if !authorized.contains(&parent_path) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "VMDK parent requires explicit authorization",
            ));
        }
        let mut factory = Discovery {
            budget: budget.clone(),
            child_identities,
            child_count,
            physical: child_physical,
            dependencies: Vec::new(),
            nodes: Vec::new(),
            node_paths: Vec::new(),
            cache: Vec::new(),
        };
        let reader = Vmdk::chain_node(
            &parent_path,
            &authorized,
            budget,
            &mut Vec::new(),
            1,
            &mut factory,
        )?;
        if reader.cid != Some(link.parent_cid) || reader.len() != sector(link.sectors)? {
            return Err(invalid("VMDK pinned parent CID or capacity mismatch"));
        }
        Ok(Some(PinnedParentGraph {
            reader: Arc::new(reader),
            dependencies: factory.dependencies,
            nodes: factory.nodes,
            budget: budget.clone(),
            _cache: factory.cache,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        Vec<PathBuf>,
        Arc<dyn ReadAt>,
        Vec<same_file::Handle>,
        u64,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent.vmdk");
        let mut extents = Vec::new();
        for index in 1..=2 {
            let path = directory.path().join(format!("parent-s{index}.vmdk"));
            let writer = crate::VmdkWriter::create(&path, 65536).unwrap();
            writer.write_all_at(0, &vec![index as u8; 65536]).unwrap();
            writer.flush().unwrap();
            drop(writer);
            let mut bytes = std::fs::read(&path).unwrap();
            let desc = u64le(&bytes, 28) as usize * 512;
            let size = u64le(&bytes, 36) as usize * 512;
            bytes[desc..desc + size].fill(0);
            std::fs::write(&path, bytes).unwrap();
            extents.push(path);
        }
        std::fs::write(&parent,"version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"parent-s1.vmdk\"\nRW 128 SPARSE \"parent-s2.vmdk\"\n").unwrap();
        let child = directory.path().join("child.vmdk");
        std::fs::write(&child,"version=1\nCID=87654321\nparentCID=12345678\nparentFileNameHint=\"parent.vmdk\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"child-s1.vmdk\"\nRW 128 SPARSE \"child-s2.vmdk\"\n").unwrap();
        let raw = crate::RawDisk::open(&child).unwrap();
        let physical = raw.len();
        let identities = vec![raw.identity().unwrap()];
        let source = Arc::new(raw);
        let mut authorized = vec![parent];
        authorized.extend(extents);
        (directory, child, authorized, source, identities, physical)
    }
    #[test]
    fn pinned_split_graph_retains_shared_read_only_sources_and_physical_topology() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, child, authorized, source, identities, physical) = graph_fixture();
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        let graph = Vmdk::resolve_pinned_parent(
            source,
            &child,
            &authorized,
            &budget,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        assert_eq!(graph.dependencies.len(), 3);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].descriptor, 0);
        assert_eq!(graph.nodes[0].extents.len(), 2);
        let mut actual = vec![0; 131072];
        graph.reader.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(&actual[..65536], &vec![1; 65536]);
        assert_eq!(&actual[65536..], &vec![2; 65536]);
        for pin in &graph.dependencies {
            pin.verify().unwrap();
            assert_eq!(
                crate::RawWriter::open(&pin.path).err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(crate::transaction::digest_reader(pin).unwrap(), pin.digest);
        }
        drop(graph);
        for path in authorized {
            drop(crate::RawWriter::open(path).unwrap());
        }
    }
    #[test]
    fn pinned_graph_refuses_denied_alias_and_live_dependency_replacement() {
        let _boundary = crate::test_sync::writer_test();
        let (directory, child, authorized, source, identities, physical) = graph_fixture();
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        assert_eq!(
            Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized[..1],
                &budget,
                &identities,
                1,
                physical
            )
            .err()
            .unwrap()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        let alias = vec![
            crate::RawDisk::open(&authorized[1])
                .unwrap()
                .identity()
                .unwrap(),
        ];
        assert!(
            Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized,
                &budget,
                &alias,
                1,
                physical
            )
            .is_err()
        );
        let graph = Vmdk::resolve_pinned_parent(
            source,
            &child,
            &authorized,
            &budget,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        let bytes = std::fs::read(&authorized[2]).unwrap();
        std::fs::rename(&authorized[2], directory.path().join("replaced.old")).unwrap();
        std::fs::write(&authorized[2], &bytes).unwrap();
        assert!(graph.verify().is_err());
        let mut actual = [0; 19];
        graph.reader.read_exact_at(65536, &mut actual).unwrap();
        assert_eq!(actual, [2; 19]);
        assert_eq!(std::fs::read(&authorized[2]).unwrap(), bytes);
    }
    #[test]
    fn frozen_child_shadow_sources_preserve_parent_link_and_never_open_unknown_extent() {
        let _boundary = crate::test_sync::writer_test();
        let (directory, child, authorized, source, identities, physical) = graph_fixture();
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        let graph = Vmdk::resolve_pinned_parent(
            source.clone(),
            &child,
            &authorized,
            &budget,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        let mut child_paths = Vec::new();
        let mut child_sources: Vec<Arc<dyn ReadAt>> = Vec::new();
        for index in 1..=2 {
            let path = directory.path().join(format!("child-s{index}.vmdk"));
            drop(crate::VmdkWriter::create_sparse(&path, 65536).unwrap());
            let mut bytes = std::fs::read(&path).unwrap();
            let desc = u64le(&bytes, 28) as usize * 512;
            let size = u64le(&bytes, 36) as usize * 512;
            bytes[desc..desc + size].fill(0);
            std::fs::write(&path, bytes).unwrap();
            child_sources.push(Arc::new(crate::RawDisk::open(&path).unwrap()));
            child_paths.push(path);
        }
        for path in std::iter::once(&child).chain(&child_paths) {
            std::fs::write(
                crate::transaction::sidecar(path),
                b"pending child shadow marker",
            )
            .unwrap();
        }
        graph
            .validate_child(source, &child, &child_sources, &child_paths)
            .unwrap();
        let mut factory = FrozenChildren {
            paths: &child_paths,
            sources: &child_sources,
            used: vec![false; 2],
        };
        let known = factory.open(&child_paths[0], &budget).unwrap();
        assert!(known.identity.is_none());
        assert!(factory.open(&authorized[1], &budget).is_err());
        assert!(factory.open(&child_paths[0], &budget).is_err());
        assert!(
            graph
                .validate_child(
                    Arc::new(crate::RawDisk::open(&child).unwrap()),
                    &child,
                    &child_sources[..1],
                    &child_paths[..1]
                )
                .is_err()
        );
    }

    #[test]
    fn hosted_parent_uses_one_manifest_file_for_descriptor_and_extent() {
        let _boundary = crate::test_sync::writer_test();
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("parent.vmdk");
        let writer = crate::VmdkWriter::create(&parent, 65536).unwrap();
        writer.write_all_at(0, &[31; 19]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        let link =
            Vmdk::hosted_link(Arc::new(crate::RawDisk::open(&parent).unwrap()), &budget).unwrap();
        let child = directory.path().join("child.vmdk");
        std::fs::write(&child,format!("version=1\nCID=12345678\nparentCID={:08x}\nparentFileNameHint=\"parent.vmdk\"\ncreateType=\"twoGbMaxExtentSparse\"\nRW 128 SPARSE \"child-s1.vmdk\"\n",link.cid)).unwrap();
        let raw = crate::RawDisk::open(&child).unwrap();
        let identity = raw.identity().unwrap();
        let length = raw.len();
        let graph = Vmdk::resolve_pinned_parent(
            Arc::new(raw),
            &child,
            &[parent],
            &budget,
            &[identity],
            1,
            length,
        )
        .unwrap()
        .unwrap();
        assert_eq!(graph.dependencies.len(), 1);
        assert_eq!(graph.nodes[0].descriptor, 0);
        assert_eq!(graph.nodes[0].extents[0].file, 0);
        assert_eq!(
            graph.nodes[0].extents[0].kind,
            DependencyExtentKind::HostedSparse
        );
        let mut actual = [0; 19];
        graph.reader.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, [31; 19]);
    }

    #[test]
    fn pinned_dependencies_refuse_live_length_and_pending_markers_without_cleanup() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, child, authorized, source, identities, physical) = graph_fixture();
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        let graph = Vmdk::resolve_pinned_parent(
            source,
            &child,
            &authorized,
            &budget,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        let child_before = std::fs::read(&child).unwrap();
        let pin = &graph.dependencies[1];
        let bytes = std::fs::read(&pin.path).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&pin.path)
            .unwrap()
            .set_len(pin.length + 512)
            .unwrap();
        assert!(graph.verify().is_err());
        std::fs::OpenOptions::new()
            .write(true)
            .open(&pin.path)
            .unwrap()
            .set_len(pin.length)
            .unwrap();
        graph.verify().unwrap();
        let marker = crate::transaction::sidecar(&pin.path);
        std::fs::write(&marker, b"foreign immutable parent marker").unwrap();
        assert!(graph.verify().is_err());
        assert_eq!(
            std::fs::read(&marker).unwrap(),
            b"foreign immutable parent marker"
        );
        assert_eq!(std::fs::read(&pin.path).unwrap(), bytes);
        assert_eq!(std::fs::read(&child).unwrap(), child_before);
    }
    #[test]
    fn read_only_parent_permissions_and_one_graph_budget_are_respected() {
        use std::os::unix::fs::PermissionsExt;
        let _boundary = crate::test_sync::writer_test();
        let (_directory, child, authorized, source, identities, physical) = graph_fixture();
        let child_before = std::fs::read(&child).unwrap();
        for path in &authorized {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
        let tight = ReadBudget::new(ParserLimits {
            metadata_bytes: 512,
            ..Default::default()
        })
        .unwrap();
        assert!(
            Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized,
                &tight,
                &identities,
                1,
                physical
            )
            .is_err()
        );
        assert!(tight.usage().metadata_bytes > 0);
        assert_eq!(std::fs::read(&child).unwrap(), child_before);
        let budget = ReadBudget::new(ParserLimits::default()).unwrap();
        let graph = Vmdk::resolve_pinned_parent(
            source,
            &child,
            &authorized,
            &budget,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        assert!(
            graph.dependencies.iter().all(|pin| pin
                .raw
                .pin_metadata()
                .unwrap()
                .permissions()
                .readonly())
        );
        let mut bytes = [0; 19];
        graph.reader.read_exact_at(65536, &mut bytes).unwrap();
        assert_eq!(bytes, [2; 19]);
        assert!(budget.usage().metadata_bytes > tight.usage().metadata_bytes);
    }
    #[test]
    fn multi_generation_graph_pins_hosted_and_flat_ancestors_at_split_offsets() {
        let _boundary = crate::test_sync::writer_test();
        for flat in [false, true] {
            let (directory, child, mut authorized, source, identities, physical) = graph_fixture();
            let grandparent = directory.path().join("grandparent.vmdk");
            let grand_cid;
            let mut expected = vec![31; 131072];
            expected[65536..].fill(32);
            let alias_path;
            if flat {
                let payload = directory.path().join("grand-flat.raw");
                let mut bytes = vec![99; 512];
                bytes.extend_from_slice(&expected);
                std::fs::write(&payload, bytes).unwrap();
                std::fs::write(&grandparent,"version=1\nCID=11223344\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 256 FLAT \"grand-flat.raw\" 1\n").unwrap();
                grand_cid = 0x11223344;
                alias_path = payload.clone();
                authorized.push(payload);
            } else {
                let writer = crate::VmdkWriter::create(&grandparent, 131072).unwrap();
                writer.write_all_at(0, &expected).unwrap();
                writer.flush().unwrap();
                drop(writer);
                grand_cid = Vmdk::hosted_link(
                    Arc::new(crate::RawDisk::open(&grandparent).unwrap()),
                    &ReadBudget::new(ParserLimits::default()).unwrap(),
                )
                .unwrap()
                .cid;
                alias_path = grandparent.clone();
            }
            authorized.push(grandparent.clone());
            let parent = authorized[0].clone();
            let descriptor = std::fs::read_to_string(&parent).unwrap().replace(
                "parentCID=ffffffff",
                &format!("parentCID={grand_cid:08x}\nparentFileNameHint=\"grandparent.vmdk\""),
            );
            std::fs::write(&parent, &descriptor).unwrap();
            for path in &authorized[1..3] {
                let mut bytes = std::fs::read(path).unwrap();
                let gd = u64le(&bytes, 56) as usize * 512;
                let gt = u32le(&bytes, gd) as usize * 512;
                bytes[gt..gt + 2048].fill(0);
                std::fs::write(path, bytes).unwrap();
            }
            let budget = ReadBudget::new(ParserLimits::default()).unwrap();
            let graph = Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized,
                &budget,
                &identities,
                1,
                physical,
            )
            .unwrap()
            .unwrap();
            assert_eq!(graph.nodes.len(), 2);
            assert_eq!(graph.dependencies.len(), if flat { 5 } else { 4 });
            let direct = &graph.nodes[0];
            let ancestor = &graph.nodes[1];
            assert_eq!(direct.descriptor, 0);
            assert_eq!(direct.cid, 0x12345678);
            assert_eq!(direct.parent_cid, grand_cid);
            assert_eq!(direct.parent, Some(1));
            assert_eq!(direct.capacity, 131072);
            assert_eq!(ancestor.parent, None);
            assert_eq!(ancestor.parent_cid, u32::MAX);
            assert_eq!(ancestor.cid, grand_cid);
            assert_eq!(ancestor.capacity, 131072);
            assert_eq!(graph.dependencies[ancestor.descriptor].path, grandparent);
            assert_eq!(direct.extents.len(), 2);
            assert_eq!(direct.extents[0].start, 0);
            assert_eq!(direct.extents[1].start, 65536);
            assert!(
                direct
                    .extents
                    .iter()
                    .all(|extent| extent.kind == DependencyExtentKind::HostedSparse
                        && extent.length == 65536)
            );
            assert_eq!(ancestor.extents.len(), 1);
            assert_eq!(
                ancestor.extents[0].kind,
                if flat {
                    DependencyExtentKind::Flat
                } else {
                    DependencyExtentKind::HostedSparse
                }
            );
            assert_eq!(ancestor.extents[0].offset, if flat { 512 } else { 0 });
            assert_eq!(ancestor.extents[0].file == ancestor.descriptor, !flat);
            let paths = graph
                .dependencies
                .iter()
                .map(|pin| pin.path.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(paths.len(), graph.dependencies.len());
            assert_eq!(paths, authorized.iter().cloned().collect());
            for pin in &graph.dependencies {
                assert_eq!(crate::transaction::digest_reader(pin).unwrap(), pin.digest);
                assert_eq!(
                    crate::RawWriter::open(&pin.path).err().unwrap().kind(),
                    io::ErrorKind::WouldBlock
                );
            }
            let mut actual = vec![0; 131072];
            graph.reader.read_exact_at(0, &mut actual).unwrap();
            assert_eq!(actual, expected);
            let before = budget.usage().work_items;
            graph.reader.budget().unwrap().work(1).unwrap();
            assert_eq!(budget.usage().work_items, before + 1);
            assert_eq!(graph.budget.usage().work_items, before + 1);
            drop(graph);
            let bytes_before = std::fs::read(&child).unwrap();
            let alias_name = alias_path.file_name().unwrap().to_str().unwrap();
            std::fs::write(&parent, descriptor.replace("parent-s1.vmdk", alias_name)).unwrap();
            let error = Vmdk::resolve_pinned_parent(
                source,
                &child,
                &authorized,
                &budget,
                &identities,
                1,
                physical,
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("alias"), "{error}");
            assert_eq!(std::fs::read(&child).unwrap(), bytes_before);
            for path in &authorized {
                assert!(!crate::transaction::pending(path).unwrap());
            }
        }
    }
    #[test]
    fn discovery_digest_scratch_respects_tight_shared_cache_budget() {
        let _boundary = crate::test_sync::writer_test();
        let (_directory, child, authorized, source, identities, physical) = graph_fixture();
        let before = std::fs::read(&child).unwrap();
        let tight = ReadBudget::new(ParserLimits {
            cache_bytes: 32768,
            ..Default::default()
        })
        .unwrap();
        assert!(
            Vmdk::resolve_pinned_parent(
                source.clone(),
                &child,
                &authorized,
                &tight,
                &identities,
                1,
                physical
            )
            .is_err(),
            "64 KiB digest scratch must not fit a 32 KiB graph cache"
        );
        assert_eq!(std::fs::read(&child).unwrap(), before);
        assert!(
            authorized
                .iter()
                .all(|path| !crate::transaction::pending(path).unwrap())
        );
        let enough = ReadBudget::new(ParserLimits {
            cache_bytes: 131072,
            ..Default::default()
        })
        .unwrap();
        let graph = Vmdk::resolve_pinned_parent(
            source,
            &child,
            &authorized,
            &enough,
            &identities,
            1,
            physical,
        )
        .unwrap()
        .unwrap();
        assert_eq!(graph.dependencies.len(), 3);
        assert!(
            enough.usage().cache_bytes < 65536,
            "digest scratch reservation must release after discovery"
        );
    }
}
