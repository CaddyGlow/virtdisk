//! Caller-owned external snapshot dependencies.
use crate::{
    Image, ImageFormat, OperationContext, OperationPhase, ParserLimits, Qcow2, RawDisk, ReadAt,
    ReadBudget, convert_image_with_context,
};
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
    budget: Option<ReadBudget>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
impl ImageGraph {
    /// Capture a validated dependency declaration and optional selected state.
    /// The selection must be registered. All live edges and identities are
    /// revalidated; callers exclude concurrent changes. Saving this declaration
    /// does not atomically commit image mutations or prove unchanged content.
    pub fn manifest(&self, selected: Option<&Path>) -> io::Result<crate::GraphManifest> {
        let selected = selected.map(Path::canonicalize).transpose()?;
        if let Some(path) = &selected {
            self.node(path)?;
        }
        for node in &self.nodes {
            self.reader(&node.spec.path)?;
        }
        let images = self.nodes.iter().map(|node| node.spec.clone()).collect();
        Ok(crate::GraphManifest::new(images, selected))
    }

    /// Save a freshly validated declaration and optional selection to a new file.
    /// Existing images and declarations are preserved. The returned declaration
    /// contains the selected registered path, or no selection when `None`.
    pub fn save_manifest(
        &self,
        selected: Option<&Path>,
        output: impl AsRef<Path>,
    ) -> io::Result<crate::GraphManifest> {
        self.save_manifest_with_context(selected, output, &mut OperationContext::default())
    }

    /// Validate and save a declaration with cancellation before validation and
    /// publication. No observer runs after publication. Existing destinations
    /// are refused; directory sync failure can leave a published manifest.
    /// Selection changes no image bytes or graph edges. Callers serialize graph
    /// updates and exclude concurrent mutation. Payload usage stays unchanged:
    /// native metadata validation and filesystem persistence are outside payload
    /// accounting, while configured graph parser budgets remain cumulative.
    pub fn save_manifest_with_context(
        &self,
        selected: Option<&Path>,
        output: impl AsRef<Path>,
        context: &mut OperationContext<'_>,
    ) -> io::Result<crate::GraphManifest> {
        context.observe_phase(OperationPhase::MetadataValidation, 0, 0)?;
        let manifest = self.manifest(selected)?;
        context.observe_phase(OperationPhase::Publication, 0, 0)?;
        manifest.save(output)?;
        Ok(manifest)
    }

    /// Validate registered paths, opened identities, dependency cycles and backing metadata.
    pub fn open(specs: &[ImageSpec]) -> io::Result<Self> {
        Self::open_inner(specs, None)
    }
    /// Open and retain one cumulative parser budget for the entire graph.
    /// Registration, identity revalidation, native graph readers and deferred
    /// reads share the same counters. Failed work retains accepted charges.
    /// Management backend allocation and native writer work remain separate.
    /// Existing [`Self::open`] keeps its per-open legacy budget behavior.
    pub fn open_with_limits(specs: &[ImageSpec], limits: ParserLimits) -> io::Result<Self> {
        Self::open_inner(specs, Some(ReadBudget::new(limits)?))
    }
    /// Shared parser accounting for an explicitly bounded graph, or `None` for
    /// legacy per-reader accounting. Readers retain this budget independently.
    pub fn budget(&self) -> Option<ReadBudget> {
        self.budget.clone()
    }
    pub(crate) fn open_inner(specs: &[ImageSpec], budget: Option<ReadBudget>) -> io::Result<Self> {
        if specs.len() > 128 {
            return Err(invalid("image graph exceeds 128 nodes"));
        }
        let mut nodes: Vec<Node> = Vec::new();
        if let Some(budget) = &budget {
            budget.metadata((specs.len() * std::mem::size_of::<Node>()) as u64)?;
        }
        nodes
            .try_reserve_exact(specs.len())
            .map_err(io::Error::other)?;
        for spec in specs {
            if let Some(budget) = &budget {
                budget.work(1)?;
                budget.metadata((spec.path.as_os_str().as_encoded_bytes().len() as u64) * 2)?;
                if let Some(parent) = &spec.parent {
                    budget.work(1)?;
                    budget.metadata((parent.as_os_str().as_encoded_bytes().len() as u64) * 2)?;
                }
            }
            let path = spec.path.canonicalize()?;
            if let Some(budget) = &budget {
                budget.metadata(path.as_os_str().as_encoded_bytes().len() as u64)?;
            }
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
                    parent: spec
                        .parent
                        .as_ref()
                        .map(|p| {
                            let canonical = p.canonicalize()?;
                            if let Some(budget) = &budget {
                                budget.metadata(
                                    canonical.as_os_str().as_encoded_bytes().len() as u64
                                )?;
                            }
                            Ok::<_, io::Error>(canonical)
                        })
                        .transpose()?,
                },
                identity,
            });
        }
        let graph = Self { nodes, budget };
        for node in &graph.nodes {
            if let Some(budget) = &graph.budget {
                budget.recursion(1, 32)?;
            }
            let mut current = node.spec.parent.as_ref();
            let mut visited = vec![&node.spec.path];
            while let Some(path) = current {
                if visited.contains(&path) || visited.len() >= 32 {
                    return Err(invalid("cyclic or excessively deep image graph"));
                }
                if let Some(budget) = &graph.budget {
                    budget.recursion(visited.len() as u128 + 1, 32)?;
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
        if let Some(budget) = &self.budget {
            budget.recursion(depth as u128 + 1, 32)?;
        }
        if depth >= 32 {
            return Err(invalid("snapshot exceeds graph depth limit"));
        }
        Ok(())
    }
    fn verify_identities(&self) -> io::Result<()> {
        for node in &self.nodes {
            if let Some(budget) = &self.budget {
                budget.work(1)?;
            }
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
    fn parser_budget(&self) -> io::Result<ReadBudget> {
        match &self.budget {
            Some(budget) => Ok(budget.clone()),
            None => ReadBudget::new(ParserLimits::default()),
        }
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
            let image = Qcow2::open_chain_with_budget(path, &authorized, self.parser_budget()?)?;
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
                    if image.resolved_backing_format() != Some(registered.spec.format) {
                        return Err(invalid(
                            "resolved parent format differs from graph declaration",
                        ));
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
                return match &self.budget {
                    Some(budget) => Ok(budget.reader(Arc::new(RawDisk::open(path)?))),
                    None => Ok(Arc::new(Image::open(path, Some(ImageFormat::Raw))?)),
                };
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
                    let image = crate::Vdi::open_chain_with_budget(
                        path,
                        &authorized,
                        self.parser_budget()?,
                    )?;
                    let parent = image.resolved_parent_path();
                    (Arc::new(image), parent)
                }
                ImageFormat::Vhdx => {
                    let image = crate::Vhdx::open_chain_with_budget(
                        path,
                        &authorized,
                        self.parser_budget()?,
                    )?;
                    let parent = image.resolved_parent_path();
                    (Arc::new(image), parent)
                }
                ImageFormat::Vmdk => {
                    let source = RawDisk::open(path)?;
                    let mut signature = [0; 4];
                    if let Some(budget) = &self.budget {
                        budget.work(1)?;
                    }
                    source.read_exact_at(0, &mut signature)?;
                    if &signature != b"KDMV" {
                        return Err(invalid(
                            "graph does not own external VMDK descriptor extents",
                        ));
                    }
                    let image = crate::Vmdk::open_chain_with_budget(
                        path,
                        &authorized,
                        self.parser_budget()?,
                    )?;
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
        self.snapshot_with_context(parent, output, &mut OperationContext::default())
    }
    /// Publish a QCOW2 external snapshot with shared verification budgets.
    ///
    /// Opening, creation, metadata validation and synchronization are backend
    /// work outside context accounting. Logical comparison is bounded and
    /// cancellable, followed by a final callback before publication. Failure
    /// removes unpublished staging on a best-effort basis and registers no edge.
    /// Directory-sync or post-publication registration failure may leave a file
    /// published without a registered edge. Exclude concurrent graph mutations.
    pub fn snapshot_with_context(
        &mut self,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
        context: &mut OperationContext<'_>,
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
        context.observe_phase(OperationPhase::MetadataValidation, 0, 0)?;
        crate::image::publish_image(output.as_ref(), |temporary| {
            crate::create_qcow2_overlay_with_chain(
                temporary,
                &parent,
                format,
                reader.len(),
                &authorized,
            )?;
            let child =
                Qcow2::open_chain_with_budget(temporary, &authorized, self.parser_budget()?)?;
            child.validate_active_mapping()?;
            if !crate::image::compare_images_in_phase(
                reader.as_ref(),
                &child,
                context,
                OperationPhase::OutputVerification,
            )? {
                return Err(invalid("snapshot content differs from parent"));
            }
            context.observe_phase(OperationPhase::Publication, 0, 0)
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
        self.snapshot_as_with_context(parent, output, format, &mut OperationContext::default())
    }
    /// Publish a native external snapshot using the context contract of
    /// [`Self::snapshot_with_context`]. Verification counts source and child reads;
    /// native metadata allocation and scratch are outside context accounting.
    pub fn snapshot_as_with_context(
        &mut self,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
        context: &mut OperationContext<'_>,
    ) -> io::Result<()> {
        if format == ImageFormat::Qcow2 {
            return self.snapshot_with_context(parent, output, context);
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
        context.observe_phase(OperationPhase::MetadataValidation, 0, 0)?;
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
                match &self.budget {
                    Some(budget) => Arc::new(crate::Vhdx::open_chain_at_with_budget(
                        temporary,
                        locator_directory,
                        &authorized,
                        budget.clone(),
                    )?),
                    None => Arc::new(crate::Vhdx::open_chain_at(
                        temporary,
                        locator_directory,
                        &authorized,
                    )?),
                }
            } else if let Some(budget) = &self.budget {
                match format {
                    ImageFormat::Vdi => Arc::new(crate::Vdi::open_chain_with_budget(
                        temporary,
                        &authorized,
                        budget.clone(),
                    )?),
                    ImageFormat::Vmdk => Arc::new(crate::Vmdk::open_chain_with_budget(
                        temporary,
                        &authorized,
                        budget.clone(),
                    )?),
                    _ => unreachable!("native snapshot family already checked"),
                }
            } else {
                Arc::new(Image::open_chain(temporary, Some(format), &authorized)?)
            };
            if !crate::image::compare_images_in_phase(
                reader.as_ref(),
                child.as_ref(),
                context,
                OperationPhase::OutputVerification,
            )? {
                return Err(invalid("snapshot content differs from parent"));
            }
            context.observe_phase(OperationPhase::Publication, 0, 0)
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
        self.flatten_with_context(path, output, format, &mut OperationContext::default())
    }
    /// Flatten with shared export, verification and publication controls.
    /// Graph opening and identity checks precede the materialization context.
    pub fn flatten_with_context(
        &self,
        path: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
        context: &mut OperationContext<'_>,
    ) -> io::Result<()> {
        convert_image_with_context(self.reader(path)?.as_ref(), output, format, context)
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
        self.rebase_to_with_context(source, parent, output, &mut OperationContext::default())
    }
    /// Rebase into a new QCOW2 child with bounded difference-copy buffers,
    /// verification and pre-publication cancellation. Originals remain immutable.
    ///
    /// The export pass preflights worst-case source reads, parent reads and native
    /// write calls. Only attempted calls are charged; skipped parent reads and
    /// unchanged chunks cost no I/O. Backend metadata, allocations, opening and
    /// flush remain outside accounting. Later quota refusal keeps earlier usage
    /// but discards unpublished staging. Publication/registration failure effects
    /// follow [`Self::snapshot_with_context`].
    pub fn rebase_to_with_context(
        &mut self,
        source: impl AsRef<Path>,
        parent: impl AsRef<Path>,
        output: impl AsRef<Path>,
        context: &mut OperationContext<'_>,
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
        let chunk = context.preflight(source.len(), 2, 3)?;
        context.observe_phase(OperationPhase::ImageExport, 0, source.len())?;
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
                let mut selected = crate::operation_context::scratch_buffer(chunk)?;
                context.scratch(chunk);
                let mut inherited = crate::operation_context::scratch_buffer(chunk)?;
                context.scratch(chunk * 2);
                let mut offset = 0;
                while offset < source.len() {
                    let count = (source.len() - offset).min(selected.len() as u64) as usize;
                    context.attempted_io();
                    source.read_exact_at(offset, &mut selected[..count])?;
                    inherited[..count].fill(0);
                    let available = backing.len().saturating_sub(offset).min(count as u64) as usize;
                    if available != 0 {
                        context.attempted_io();
                        backing.read_exact_at(offset, &mut inherited[..available])?;
                    }
                    if selected[..count] != inherited[..count] {
                        context.attempted_io();
                        writer.write_all_at(offset, &selected[..count])?;
                    }
                    offset += count as u64;
                    context.completed(count as u64);
                    context.observe_phase(OperationPhase::ImageExport, offset, source.len())?;
                }
                writer.flush()?;
            }
            let reopened =
                Qcow2::open_chain_with_budget(temporary, &authorized, self.parser_budget()?)?;
            context.observe_phase(OperationPhase::MetadataValidation, 0, 0)?;
            reopened.validate_active_mapping()?;
            if !crate::image::compare_images_in_phase(
                source.as_ref(),
                &reopened,
                context,
                OperationPhase::OutputVerification,
            )? {
                return Err(invalid("rebased content differs from source"));
            }
            context.observe_phase(OperationPhase::Publication, 0, 0)
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
        self.merge_to_with_context(
            child,
            ancestor,
            output,
            format,
            &mut OperationContext::default(),
        )
    }
    /// Materialize a descendant with shared controls after validating ancestry.
    /// Original branches remain immutable; this is a new-output merge.
    pub fn merge_to_with_context(
        &self,
        child: impl AsRef<Path>,
        ancestor: impl AsRef<Path>,
        output: impl AsRef<Path>,
        format: ImageFormat,
        context: &mut OperationContext<'_>,
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
        self.flatten_with_context(&child, output, format, context)
    }
    /// Remove a registered leaf snapshot under the caller's complete-graph ownership declaration.
    /// Base images and snapshots with children are refused. Directory sync failure
    /// can be reported after the file has already been removed.
    pub fn delete_snapshot(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
        self.delete_snapshot_with_context(path, &mut OperationContext::default())
    }

    /// Delete an owned registered leaf with cancellation before validation and
    /// before acquiring its exclusive handle and unlinking it. Identities are
    /// checked again after the final `SnapshotDeletion` callback. No observer
    /// runs after unlink. Directory sync failure can leave the file removed and
    /// the live graph updated. Native metadata/locking/persistence consume no
    /// payload budget; configured graph parser accounting remains cumulative.
    /// Callers declare complete dependency ownership and exclude concurrent
    /// mutation. Base images, non-leaves and external VMDK descriptor ownership
    /// remain outside this deletion contract.
    pub fn delete_snapshot_with_context(
        &mut self,
        path: impl AsRef<Path>,
        context: &mut OperationContext<'_>,
    ) -> io::Result<()> {
        self.delete_snapshot_with_sync(path.as_ref(), context, sync_deleted_parent)
    }

    fn delete_snapshot_with_sync(
        &mut self,
        path: &Path,
        context: &mut OperationContext<'_>,
        sync_parent: fn(&Path) -> io::Result<()>,
    ) -> io::Result<()> {
        context.observe_phase(OperationPhase::MetadataValidation, 0, 0)?;
        let path = path.canonicalize()?;
        for node in &self.nodes {
            self.reader(&node.spec.path)?;
        }
        {
            let _validated = self.reader(&path)?;
        }
        if self.node(&path)?.spec.parent.is_none() || !self.children(&path)?.is_empty() {
            return Err(invalid("only dependent leaf snapshots may be deleted"));
        }
        context.observe_phase(OperationPhase::SnapshotDeletion, 0, 0)?;
        let _exclusive = crate::RawWriter::open(&path)?;
        self.verify_identities()?;
        std::fs::remove_file(&path)?;
        self.nodes.retain(|n| n.spec.path != path);
        sync_parent(&path)?;
        Ok(())
    }
}

fn sync_deleted_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("deleted snapshot requires a parent directory"))?;
        std::fs::File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod deletion_tests {
    use super::*;

    #[test]
    fn directory_sync_failure_keeps_leaf_deleted_and_graph_updated() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base");
        let child = dir.path().join("child");
        let sibling = dir.path().join("sibling");
        std::fs::write(&base, [37; 512]).unwrap();
        let mut graph = ImageGraph::open(&[ImageSpec {
            path: base.clone(),
            format: ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        graph.snapshot(&base, &child).unwrap();
        graph.snapshot(&base, &sibling).unwrap();
        let error = graph
            .delete_snapshot_with_sync(&child, &mut OperationContext::default(), |_| {
                Err(io::Error::other("injected deletion sync failure"))
            })
            .unwrap_err();
        assert_eq!(error.to_string(), "injected deletion sync failure");
        assert!(!child.exists());
        assert_eq!(
            graph.children(&base).unwrap().as_slice(),
            std::slice::from_ref(&sibling)
        );
        let manifest = graph.manifest(Some(&sibling)).unwrap();
        assert_eq!(manifest.images().len(), 2);
        let mut bytes = [0; 512];
        graph
            .reader(&sibling)
            .unwrap()
            .read_exact_at(0, &mut bytes)
            .unwrap();
        assert_eq!(bytes, [37; 512]);
        assert_eq!(std::fs::read(base).unwrap(), [37; 512]);
    }
}

#[path = "graph_generation.rs"]
mod generation;
