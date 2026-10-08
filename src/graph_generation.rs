//! Atomic publication of fresh external snapshot and rebase generations.
use super::ImageGraph;
use crate::{GraphManifest, ImageFormat, OperationContext};
use std::{io, path::Path};

impl ImageGraph {
    /// Publish a fresh directory containing `image` and `graph.manifest`.
    /// The new external snapshot is selected in the saved declaration and
    /// registered in this graph. Source states and existing generations remain
    /// immutable. See [`Self::snapshot_generation_with_context`] for guarantees.
    pub fn snapshot_generation(
        &mut self,
        parent: impl AsRef<Path>,
        directory: impl AsRef<Path>,
        format: ImageFormat,
    ) -> io::Result<GraphManifest> {
        self.snapshot_generation_with_context(
            parent,
            directory,
            format,
            &mut OperationContext::default(),
        )
    }
    /// Publish image and manifest together through a no-overwrite directory rename.
    ///
    /// Linux `RENAME_NOREPLACE` is required; other platforms refuse before I/O.
    /// Native snapshot creation/verification and manifest serialization finish in
    /// an owned private sibling directory. Both files and the staging directory
    /// are synced before the final `GenerationPublication` cancellation boundary.
    /// Cancellation/errors before rename clean staging on a best-effort basis
    /// and leave registered dependencies unchanged. Process termination may leave
    /// private staging; it cannot expose a partial final generation.
    ///
    /// After successful rename, the new dependency is registered before syncing
    /// the parent directory. A later sync error leaves a visible complete
    /// generation and registered child. Publication cannot be revoked by callbacks.
    /// Callers exclude concurrent image and directory mutation; this captures
    /// disk state only and does not establish actual power-loss correctness.
    /// Budgets retain accepted work; native creation and filesystem sync/rename
    /// remain outside operation payload accounting.
    pub fn snapshot_generation_with_context(
        &mut self,
        parent: impl AsRef<Path>,
        directory: impl AsRef<Path>,
        format: ImageFormat,
        context: &mut OperationContext<'_>,
    ) -> io::Result<GraphManifest> {
        #[cfg(target_os = "linux")]
        {
            self.publish_generation(
                parent.as_ref(),
                directory.as_ref(),
                GenerationImage::Snapshot(format),
                context,
                std::fs::File::sync_all,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (parent, directory, format, context);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "snapshot generation publication requires Linux RENAME_NOREPLACE",
            ))
        }
    }
    /// Publish a rebased QCOW2 image and its selected manifest together.
    /// Original graph images and declarations remain unchanged.
    pub fn rebase_generation(
        &mut self,
        source: impl AsRef<Path>,
        parent: impl AsRef<Path>,
        directory: impl AsRef<Path>,
    ) -> io::Result<GraphManifest> {
        self.rebase_generation_with_context(
            source,
            parent,
            directory,
            &mut OperationContext::default(),
        )
    }

    /// Create a new QCOW2 child over a registered raw/QCOW2 parent while
    /// preserving the registered source's logical content. Difference copying
    /// and full verification use the supplied context. The image and selected
    /// manifest share the publication, cleanup, post-commit error and Linux-only
    /// guarantees of [`Self::snapshot_generation_with_context`]. This does not
    /// change existing branch edges or replace an existing generation.
    pub fn rebase_generation_with_context(
        &mut self,
        source: impl AsRef<Path>,
        parent: impl AsRef<Path>,
        directory: impl AsRef<Path>,
        context: &mut OperationContext<'_>,
    ) -> io::Result<GraphManifest> {
        #[cfg(target_os = "linux")]
        {
            self.publish_generation(
                parent.as_ref(),
                directory.as_ref(),
                GenerationImage::Rebase(source.as_ref()),
                context,
                std::fs::File::sync_all,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (source, parent, directory, context);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "rebase generation publication requires Linux RENAME_NOREPLACE",
            ))
        }
    }
    #[cfg(target_os = "linux")]
    fn publish_generation(
        &mut self,
        parent: &Path,
        directory: &Path,
        image: GenerationImage<'_>,
        context: &mut OperationContext<'_>,
        sync_parent: fn(&std::fs::File) -> io::Result<()>,
    ) -> io::Result<GraphManifest> {
        use crate::OperationPhase;
        let format = match image {
            GenerationImage::Snapshot(format) => format,
            GenerationImage::Rebase(_) => ImageFormat::Qcow2,
        };
        use std::{fs, os::unix::fs::DirBuilderExt};
        if self.nodes.len() >= 128 {
            return Err(super::invalid("image graph exceeds 128 nodes"));
        }
        let parent = parent.canonicalize()?;
        let parent_format = self.node(&parent)?.spec.format;
        let permitted = match format {
            ImageFormat::Qcow2 => matches!(parent_format, ImageFormat::Raw | ImageFormat::Qcow2),
            ImageFormat::Vhdx | ImageFormat::Vdi | ImageFormat::Vmdk => parent_format == format,
            ImageFormat::Raw => false,
        };
        if !permitted {
            return Err(super::invalid(
                "unsupported snapshot generation parent family",
            ));
        }
        self.check_child_depth(&parent)?;
        let name = directory
            .file_name()
            .ok_or_else(|| super::invalid("generation directory requires a final name"))?;
        let root = directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        let destination = root.join(name);
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "generation already exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.nodes.try_reserve(1).map_err(io::Error::other)?;
        let declaration = self.manifest(None)?;
        let mut working = Self::open_inner(declaration.images(), self.budget.clone())?;
        let root_handle = fs::File::open(&root)?;
        let mut entropy = [0; 16];
        getrandom::fill(&mut entropy).map_err(|error| io::Error::other(error.to_string()))?;
        let nonce: String = entropy.iter().map(|byte| format!("{byte:02x}")).collect();
        let stage_name = format!(".virtdisk-generation-{nonce}");
        let stage_path = root.join(&stage_name);
        fs::DirBuilder::new().mode(0o700).create(&stage_path)?;
        let mut stage = GenerationStage {
            path: stage_path,
            published: false,
        };
        let staged_image = stage.path.join("image");
        match image {
            GenerationImage::Snapshot(format) => {
                working.snapshot_as_with_context(&parent, &staged_image, format, context)?
            }
            GenerationImage::Rebase(source) => {
                working.rebase_to_with_context(source, &parent, &staged_image, context)?
            }
        }
        let final_image = destination.join("image");
        let manifest = working
            .manifest(Some(&staged_image))?
            .published_leaf(&staged_image, &final_image)?;
        manifest.save(stage.path.join("graph.manifest"))?;
        fs::File::open(&stage.path)?.sync_all()?;
        context.observe_phase(OperationPhase::GenerationPublication, 0, 0)?;
        self.verify_identities()?;
        // The child was just created and is the only appended node. Retain its
        // opened identity through rename, and reserve registration before commit.
        let mut child = working
            .nodes
            .pop()
            .ok_or_else(|| io::Error::other("missing staged generation child"))?;
        child.spec.path = final_image;
        rustix::fs::renameat_with(
            &root_handle,
            stage_name.as_str(),
            &root_handle,
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        stage.published = true;
        self.nodes.push(child);
        sync_parent(&root_handle)?;
        Ok(manifest)
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
enum GenerationImage<'a> {
    Snapshot(ImageFormat),
    Rebase(&'a Path),
}

#[cfg(target_os = "linux")]
struct GenerationStage {
    path: std::path::PathBuf,
    published: bool,
}
#[cfg(target_os = "linux")]
impl Drop for GenerationStage {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::ImageSpec;

    #[test]
    fn failed_parent_sync_keeps_complete_generation_registered() {
        for rebase in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("parent");
            let output = dir.path().join("generation");
            std::fs::write(&parent, [37; 512]).unwrap();
            let mut graph = ImageGraph::open(&[ImageSpec {
                path: parent.clone(),
                format: ImageFormat::Raw,
                parent: None,
            }])
            .unwrap();
            let image = if rebase {
                GenerationImage::Rebase(&parent)
            } else {
                GenerationImage::Snapshot(ImageFormat::Qcow2)
            };
            let error = graph
                .publish_generation(
                    &parent,
                    &output,
                    image,
                    &mut OperationContext::default(),
                    |_| Err(io::Error::other("injected parent sync failure")),
                )
                .unwrap_err();
            assert_eq!(error.to_string(), "injected parent sync failure");
            let image = output.join("image");
            assert_eq!(
                graph.children(&parent).unwrap().as_slice(),
                std::slice::from_ref(&image)
            );
            let manifest = GraphManifest::open(output.join("graph.manifest")).unwrap();
            assert_eq!(manifest.selected(), Some(image.as_path()));
            let reopened = manifest
                .open_graph(&[parent.clone(), image.clone()])
                .unwrap();
            let mut bytes = [0; 512];
            reopened
                .reader(&image)
                .unwrap()
                .read_exact_at(0, &mut bytes)
                .unwrap();
            assert_eq!(bytes, [37; 512]);
            graph
                .reader(&image)
                .unwrap()
                .read_exact_at(0, &mut bytes)
                .unwrap();
            assert_eq!(bytes, [37; 512]);
            assert_eq!(std::fs::read(&parent).unwrap(), [37; 512]);
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        }
    }
}
