//! Caller-owned external snapshot dependencies.
use crate::{Image, ImageFormat, Qcow2, RawDisk, ReadAt, convert_image};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// One image and its explicitly declared direct parent.
#[derive(Clone, Debug)]
pub struct ImageSpec {
    /// Regular image file.
    pub path: PathBuf,
    /// Explicit container family.
    pub format: ImageFormat,
    /// Direct backing image; must be registered in the same graph.
    pub parent: Option<PathBuf>,
}
struct Node {
    spec: ImageSpec,
    identity: same_file::Handle,
}
/// An explicitly authorized offline dependency graph, bounded to 128 images.
///
/// Callers must supply every dependent image and exclude concurrent file,
/// directory and image mutations throughout management operations. Filesystem
/// discovery cannot prove absence of children elsewhere. Parents remain immutable.
/// Native edges use matching container families; QCOW2 also accepts raw parents.
/// External VMDK descriptor extent ownership is not supported.
pub struct ImageGraph {
    nodes: Vec<Node>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
impl ImageGraph {
    /// Validate registered paths, opened identities, dependency cycles and backing metadata.
    pub fn open(specs: &[ImageSpec]) -> io::Result<Self> {
        if specs.len() > 128 {
            return Err(invalid("image graph exceeds 128 nodes"));
        }
        let mut nodes: Vec<Node> = Vec::new();
        for spec in specs {
            let path = spec.path.canonicalize()?;
            let identity = RawDisk::open(&path)?.identity()?;
            if nodes
                .iter()
                .any(|n| n.spec.path == path || n.identity == identity)
            {
                return Err(invalid("duplicate or aliased image"));
            }
            nodes.push(Node {
                spec: ImageSpec {
                    path,
                    format: spec.format,
                    parent: spec.parent.as_ref().map(|p| p.canonicalize()).transpose()?,
                },
                identity,
            });
        }
        let graph = Self { nodes };
        for node in &graph.nodes {
            let mut current = node.spec.parent.as_ref();
            let mut visited = vec![&node.spec.path];
            while let Some(path) = current {
                if visited.contains(&path) || visited.len() >= 32 {
                    return Err(invalid("cyclic or excessively deep image graph"));
                }
                visited.push(path);
                current = graph.node(path)?.spec.parent.as_ref();
            }
            graph.reader(&node.spec.path)?;
        }
        Ok(graph)
    }
    fn node(&self, path: &Path) -> io::Result<&Node> {
        self.nodes
            .iter()
            .find(|n| n.spec.path == path)
            .ok_or_else(|| invalid("image is not registered"))
    }
    fn authorized(&self) -> Vec<PathBuf> {
        self.nodes.iter().map(|n| n.spec.path.clone()).collect()
    }
    fn check_child_depth(&self, parent: &Path) -> io::Result<()> {
        let mut depth = 1;
        let mut current = self.node(parent)?.spec.parent.as_ref();
        while let Some(path) = current {
            depth += 1;
            current = self.node(path)?.spec.parent.as_ref();
        }
        if depth >= 32 {
            return Err(invalid("snapshot exceeds graph depth limit"));
        }
        Ok(())
    }
    fn verify_identities(&self) -> io::Result<()> {
        for node in &self.nodes {
            if RawDisk::open(&node.spec.path)?.identity()? != node.identity {
                return Err(invalid("registered image was replaced"));
            }
        }
        Ok(())
    }
    /// Open the selected state with only registered ancestors authorized.
    pub fn reader(&self, path: impl AsRef<Path>) -> io::Result<Arc<dyn ReadAt>> {
        self.verify_identities()?;
        let path = path.as_ref().canonicalize()?;
        let mut ancestors = Vec::new();
        let mut current = self.node(&path)?.spec.parent.as_ref();
        while let Some(parent) = current {
            ancestors.push(parent.clone());
            current = self.node(parent)?.spec.parent.as_ref();
        }
        for ancestor in ancestors.iter().rev() {
            self.reader_node(ancestor)?;
        }
        self.reader_node(&path)
    }
    fn reader_node(&self, path: &Path) -> io::Result<Arc<dyn ReadAt>> {
        let node = self.node(path)?;
        let mut authorized = Vec::new();
        let mut current = node.spec.parent.as_ref();
        while let Some(parent) = current {
            authorized.push(parent.clone());
            current = self.node(parent)?.spec.parent.as_ref();
        }
        if node.spec.format == ImageFormat::Qcow2 {
            let image = Qcow2::open_chain(path, &authorized)?;
            let actual = image
                .declared_backing()
                .map(|(name, format)| {
                    let reference = Path::new(name);
                    let parent = if reference.is_absolute() {
                        reference.to_path_buf()
                    } else {
                        path.parent().unwrap().join(reference)
                    }
                    .canonicalize()?;
                    let registered = self.node(&parent)?;
                    let expected = match registered.spec.format {
                        ImageFormat::Raw => "raw",
                        ImageFormat::Qcow2 => "qcow2",
                        _ => return Err(invalid("unsupported backing family")),
                    };
                    if format.is_some_and(|f| f != expected) {
                        return Err(invalid("declared parent format mismatch"));
                    }
                    Ok(parent)
                })
                .transpose()?;
            if actual != node.spec.parent {
                return Err(invalid("declared parent differs from container backing"));
            }
            image.validate_active_mapping()?;
            Ok(Arc::new(image))
        } else {
            if node.spec.format == ImageFormat::Raw {
                if node.spec.parent.is_some() {
                    return Err(invalid("raw image cannot have a parent"));
                }
                return Ok(Arc::new(Image::open(path, Some(ImageFormat::Raw))?));
            }
            if let Some(parent) = &node.spec.parent
                && self.node(parent)?.spec.format != node.spec.format
            {
                return Err(invalid(
                    "native child requires the same parent container family",
                ));
            }
            let (reader, actual): (Arc<dyn ReadAt>, Option<PathBuf>) = match node.spec.format {
                ImageFormat::Vdi => {
                    let image = crate::Vdi::open_chain(path, &authorized)?;
                    let parent = image.resolved_parent_path();
                    (Arc::new(image), parent)
                }
                ImageFormat::Vhdx => {
                    let image = crate::Vhdx::open_chain(path, &authorized)?;
                    let parent = image.resolved_parent_path();
                    (Arc::new(image), parent)
                }
                ImageFormat::Vmdk => {
                    let source = RawDisk::open(path)?;
                    let mut signature = [0; 4];
                    source.read_exact_at(0, &mut signature)?;
                    if &signature != b"KDMV" {
                        return Err(invalid(
                            "graph does not own external VMDK descriptor extents",
                        ));
                    }
                    let image = crate::Vmdk::open_chain(path, &authorized)?;
                    let parent = image.resolved_parent_path();
                    (Arc::new(image), parent)
                }
                _ => unreachable!(),
            };
            let actual = actual.map(|p| p.canonicalize()).transpose()?;
            if actual != node.spec.parent {
                return Err(invalid(
                    "declared parent differs from resolved native parent",
                ));
            }
            Ok(reader)
        }
    }

    /// List registered direct children in canonical path order.
    pub fn children(&self, path: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
        let path = path.as_ref().canonicalize()?;
        self.node(&path)?;
        let mut children: Vec<_> = self
            .nodes
            .iter()
            .filter(|n| n.spec.parent.as_ref() == Some(&path))
            .map(|n| n.spec.path.clone())
            .collect();
        children.sort();
        Ok(children)
    }
    /// Publish an empty external QCOW2 snapshot and register its parent edge.
    /// Existing destinations are never overwritten. This captures disk content only.
    pub fn snapshot(
        &mut self,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
    ) -> io::Result<()> {
        if self.nodes.len() >= 128 {
            return Err(invalid("image graph exceeds 128 nodes"));
        }
        let parent = parent.as_ref().canonicalize()?;
        self.check_child_depth(&parent)?;
        let reader = self.reader(&parent)?;
        let format = match self.node(&parent)?.spec.format {
            ImageFormat::Raw => "raw",
            ImageFormat::Qcow2 => "qcow2",
            _ => return Err(invalid("unsupported snapshot parent family")),
        };
        let authorized = self.authorized();
        crate::image::publish_image(output.as_ref(), |temporary| {
            crate::create_qcow2_overlay_with_chain(
                temporary,
                &parent,
                format,
                reader.len(),
                &authorized,
            )?;
            let child = Qcow2::open_chain(temporary, &authorized)?;
            child.validate_active_mapping()?;
            if !crate::compare_images(reader.as_ref(), &child)? {
                return Err(invalid("snapshot content differs from parent"));
            }
            Ok(())
        })?;
        let path = output.as_ref().canonicalize()?;
        let identity = RawDisk::open(&path)?.identity()?;
        self.nodes.push(Node {
            spec: ImageSpec {
                path,
                format: ImageFormat::Qcow2,
                parent: Some(parent),
            },
            identity,
        });
        Ok(())
    }
    /// Publish and register a native external disk snapshot in the selected family.
    ///
    /// Native VDI, VHDX and single-file hosted VMDK require a matching parent
    /// family. QCOW2 accepts raw or QCOW2 parents. Files remain immutable during
    /// publication; existing destinations are never overwritten. VM state is not captured.
    pub fn snapshot_as(
        &mut self,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
    ) -> io::Result<()> {
        if format == ImageFormat::Qcow2 {
            return self.snapshot(parent, output);
        }
        if self.nodes.len() >= 128 {
            return Err(invalid("image graph exceeds 128 nodes"));
        }
        let parent = parent.as_ref().canonicalize()?;
        if !matches!(
            format,
            ImageFormat::Vdi | ImageFormat::Vhdx | ImageFormat::Vmdk
        ) || self.node(&parent)?.spec.format != format
        {
            return Err(invalid(
                "native snapshot requires a matching supported parent family",
            ));
        }
        self.check_child_depth(&parent)?;
        let reader = self.reader(&parent)?;
        let mut ancestors = Vec::new();
        let mut current = self.node(&parent)?.spec.parent.as_ref();
        while let Some(path) = current {
            ancestors.push(path.clone());
            current = self.node(path)?.spec.parent.as_ref();
        }
        let mut authorized = vec![parent.clone()];
        authorized.extend_from_slice(&ancestors);
        let locator_directory = output
            .as_ref()
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        crate::image::publish_image(output.as_ref(), |temporary| {
            match format {
                ImageFormat::Vdi => {
                    let writer = crate::VdiWriter::create_overlay(temporary, &parent, &ancestors)?;
                    writer.flush()?;
                }
                ImageFormat::Vhdx => {
                    crate::vhdx_write::create_vhdx_overlay_at(
                        temporary,
                        &parent,
                        &ancestors,
                        locator_directory,
                    )?;
                }
                ImageFormat::Vmdk => {
                    let writer =
                        crate::VmdkWriter::create_overlay(temporary, &parent, &authorized)?;
                    writer.flush()?;
                }
                _ => unreachable!(),
            }
            let child: Arc<dyn ReadAt> = if format == ImageFormat::Vhdx {
                Arc::new(crate::Vhdx::open_chain_at(
                    temporary,
                    locator_directory,
                    &authorized,
                )?)
            } else {
                Arc::new(Image::open_chain(temporary, Some(format), &authorized)?)
            };
            if !crate::compare_images(reader.as_ref(), child.as_ref())? {
                return Err(invalid("snapshot content differs from parent"));
            }
            Ok(())
        })?;
        let path = output.as_ref().canonicalize()?;
        let identity = RawDisk::open(&path)?.identity()?;
        self.nodes.push(Node {
            spec: ImageSpec {
                path,
                format,
                parent: Some(parent),
            },
            identity,
        });
        Ok(())
    }
    /// Materialize the selected state as a new independent image.
    pub fn flatten(
        &self,
        path: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
    ) -> io::Result<()> {
        convert_image(self.reader(path)?.as_ref(), output, format)
    }
    /// Publish a new overlay over another registered parent, preserving selected content.
    /// Differences are materialized; both original states remain immutable.
    /// Linux journal persistence is required for payload allocation.
    pub fn rebase_to(
        &mut self,
        source: impl AsRef<Path>,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
    ) -> io::Result<()> {
        if self.nodes.len() >= 128 {
            return Err(invalid("image graph exceeds 128 nodes"));
        }
        let source = self.reader(source)?;
        let parent = parent.as_ref().canonicalize()?;
        self.check_child_depth(&parent)?;
        let backing = self.reader(&parent)?;
        let format = match self.node(&parent)?.spec.format {
            ImageFormat::Raw => "raw",
            ImageFormat::Qcow2 => "qcow2",
            _ => return Err(invalid("unsupported rebase parent family")),
        };
        let authorized = self.authorized();
        crate::image::publish_image(output.as_ref(), |temporary| {
            crate::create_qcow2_overlay_with_chain(
                temporary,
                &parent,
                format,
                source.len(),
                &authorized,
            )?;
            {
                let writer = crate::Qcow2Writer::open_chain(temporary, &authorized)?;
                let mut selected = vec![0; 65536];
                let mut inherited = vec![0; 65536];
                let mut offset = 0;
                while offset < source.len() {
                    let count = (source.len() - offset).min(selected.len() as u64) as usize;
                    source.read_exact_at(offset, &mut selected[..count])?;
                    inherited[..count].fill(0);
                    let available = backing.len().saturating_sub(offset).min(count as u64) as usize;
                    if available != 0 {
                        backing.read_exact_at(offset, &mut inherited[..available])?;
                    }
                    if selected[..count] != inherited[..count] {
                        writer.write_all_at(offset, &selected[..count])?;
                    }
                    offset += count as u64;
                }
                writer.flush()?;
            }
            let reopened = Qcow2::open_chain(temporary, &authorized)?;
            reopened.validate_active_mapping()?;
            if !crate::compare_images(source.as_ref(), &reopened)? {
                return Err(invalid("rebased content differs from source"));
            }
            Ok(())
        })?;
        let path = output.as_ref().canonicalize()?;
        let identity = RawDisk::open(&path)?.identity()?;
        self.nodes.push(Node {
            spec: ImageSpec {
                path,
                format: ImageFormat::Qcow2,
                parent: Some(parent),
            },
            identity,
        });
        Ok(())
    }
    /// Materialize a descendant state into a new output after checking ancestry.
    /// Original ancestors and sibling branches remain immutable.
    pub fn merge_to(
        &self,
        child: impl AsRef<Path>,
        ancestor: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
    ) -> io::Result<()> {
        let child = child.as_ref().canonicalize()?;
        let ancestor = ancestor.as_ref().canonicalize()?;
        self.node(&ancestor)?;
        let mut current = self.node(&child)?.spec.parent.as_ref();
        let mut found = false;
        while let Some(path) = current {
            if path == &ancestor {
                found = true;
                break;
            }
            current = self.node(path)?.spec.parent.as_ref();
        }
        if !found {
            return Err(invalid("merge destination is not an ancestor"));
        }
        self.flatten(&child, output, format)
    }
    /// Remove a registered leaf snapshot under the caller's complete-graph ownership declaration.
    /// Base images and snapshots with children are refused. Directory sync failure
    /// can be reported after the file has already been removed.
    pub fn delete_snapshot(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref().canonicalize()?;
        for node in &self.nodes {
            self.reader(&node.spec.path)?;
        }
        {
            let _validated = self.reader(&path)?;
        }
        if self.node(&path)?.spec.parent.is_none() || !self.children(&path)?.is_empty() {
            return Err(invalid("only dependent leaf snapshots may be deleted"));
        }
        let _exclusive = crate::RawWriter::open(&path)?;
        self.verify_identities()?;
        std::fs::remove_file(&path)?;
        self.nodes.retain(|n| n.spec.path != path);
        #[cfg(unix)]
        std::fs::File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}
