//! Atomic, expected-declaration replacement under a retained file lock.
use super::GraphManifest;
use crate::OperationContext;
use std::{io, path::Path};

impl GraphManifest {
    /// Atomically replace a declaration only if it still matches `expected`.
    /// See [`Self::replace_with_context`] for persistence and ownership rules.
    pub fn replace(&self, path: impl AsRef<Path>, expected: &Self) -> io::Result<()> {
        self.replace_with_context(path, expected, &mut OperationContext::default())
    }
    /// Sync a private sibling file before atomically replacing an existing manifest.
    ///
    /// Linux only; other platforms refuse before I/O or callbacks. The old file
    /// must be regular, singly linked, unlocked and declare the same state as `expected`.
    /// A retained exclusive lock, final identity/content checks and an expected
    /// declaration reject stale updates. Callers must serialize graph management
    /// and exclude noncooperating file/directory changes. This declaration grants
    /// no image authority and does not atomically commit image changes.
    ///
    /// `ManifestReplacement` permits cancellation after staging sync and before
    /// rename. No callback runs after rename. Pre-rename errors keep the destination
    /// unchanged by this operation and remove staging on a best-effort basis.
    /// Process termination may leave private staging. Parent sync failure after
    /// rename leaves the complete new declaration visible; inspect before retrying.
    /// Replacements have private 0600 permissions, without preserving old metadata.
    /// Metadata/filesystem work is outside payload quotas; usage remains unchanged.
    pub fn replace_with_context(
        &self,
        path: impl AsRef<Path>,
        expected: &Self,
        context: &mut OperationContext<'_>,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            self.replace_inner(path.as_ref(), expected, context, std::fs::File::sync_all)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, expected, context);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "atomic manifest replacement requires Linux",
            ))
        }
    }
    #[cfg(target_os = "linux")]
    fn replace_inner(
        &self,
        path: &Path,
        expected: &Self,
        context: &mut OperationContext<'_>,
        sync_parent: fn(&std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        let prepared = LockedManifest::open(path, expected)?.prepare(self)?;
        context.observe_phase(crate::OperationPhase::ManifestReplacement, 0, 0)?;
        prepared.publish()?.sync(sync_parent)
    }
}

#[cfg(target_os = "linux")]
use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, statat};
#[cfg(target_os = "linux")]
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{Seek, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

/// A validated old declaration whose source and parent handles remain owned.
/// Consuming preparation transfers both handles into the publication state.
#[cfg(target_os = "linux")]
pub(crate) struct LockedManifest {
    directory: File,
    parent: PathBuf,
    name: OsString,
    source: File,
    original: Metadata,
    original_bytes: Vec<u8>,
}
#[cfg(target_os = "linux")]
impl LockedManifest {
    pub(crate) fn open(path: &Path, expected: &GraphManifest) -> io::Result<Self> {
        let expected_bytes = expected.encode()?;
        let name = path
            .file_name()
            .ok_or_else(|| stale("manifest path has no filename"))?
            .to_owned();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        if expected
            .images()
            .iter()
            .any(|image| image.path == parent.join(&name))
        {
            return Err(stale("manifest replacement overlaps a declared image"));
        }
        let directory = File::open(&parent)?;
        let mut source = File::from(openat(
            &directory,
            &name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        source.try_lock().map_err(io::Error::from)?;
        let original = source.metadata()?;
        if !original.is_file() || original.nlink() != 1 {
            return Err(stale(
                "replacement requires a singly linked regular manifest",
            ));
        }
        let original_bytes = super::read_bytes(&mut source)?;
        if GraphManifest::decode(&original_bytes)?.encode()? != expected_bytes {
            return Err(stale("manifest no longer matches expected declaration"));
        }
        Ok(Self {
            directory,
            parent,
            name,
            source,
            original,
            original_bytes,
        })
    }

    pub(crate) fn prepare(self, successor: &GraphManifest) -> io::Result<PreparedManifest> {
        let bytes = successor.encode()?;
        if successor
            .images()
            .iter()
            .any(|image| image.path == self.parent.join(&self.name))
        {
            return Err(stale("manifest replacement overlaps a declared image"));
        }
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|error| io::Error::other(error.to_string()))?;
        let temporary = OsString::from(format!(
            ".virtdisk-manifest-{:032x}.tmp",
            u128::from_le_bytes(nonce)
        ));
        let file = File::from(openat(
            &self.directory,
            &temporary,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let mut prepared = PreparedManifest {
            locked: self,
            temporary: Some(temporary),
            file,
            bytes,
        };
        prepared.file.try_lock().map_err(io::Error::from)?;
        prepared.file.write_all(&prepared.bytes)?;
        prepared.file.sync_all()?;
        Ok(prepared)
    }

    fn validate(&mut self) -> io::Result<()> {
        let current = statat(&self.directory, &self.name, AtFlags::SYMLINK_NOFOLLOW)?;
        let live_directory = std::fs::metadata(&self.parent)?;
        let retained_directory = self.directory.metadata()?;
        if current.st_dev != self.original.dev()
            || current.st_ino != self.original.ino()
            || current.st_nlink != 1
            || live_directory.dev() != retained_directory.dev()
            || live_directory.ino() != retained_directory.ino()
        {
            return Err(stale("manifest or parent directory identity changed"));
        }
        self.source.rewind()?;
        if super::read_bytes(&mut self.source)? != self.original_bytes {
            return Err(stale("manifest content changed before replacement"));
        }
        Ok(())
    }
}

/// Synced private successor carrying ownership of the original manifest lock.
/// Publishing consumes this value; dropping it only cleans owned staging.
#[cfg(target_os = "linux")]
pub(crate) struct PreparedManifest {
    locked: LockedManifest,
    temporary: Option<OsString>,
    file: File,
    bytes: Vec<u8>,
}
#[cfg(target_os = "linux")]
impl PreparedManifest {
    pub(crate) fn publish(mut self) -> io::Result<PublishedManifest> {
        self.locked.validate()?;
        let temporary = self
            .temporary
            .as_deref()
            .ok_or_else(|| stale("manifest staging has already been published"))?;
        let staged = statat(&self.locked.directory, temporary, AtFlags::SYMLINK_NOFOLLOW)?;
        let retained = self.file.metadata()?;
        if staged.st_dev != retained.dev()
            || staged.st_ino != retained.ino()
            || staged.st_nlink != 1
        {
            return Err(stale("manifest staging identity changed"));
        }
        self.file.rewind()?;
        if super::read_bytes(&mut self.file)? != self.bytes {
            return Err(stale("manifest staging content changed"));
        }
        renameat(
            &self.locked.directory,
            temporary,
            &self.locked.directory,
            &self.locked.name,
        )?;
        self.temporary = None;
        Ok(PublishedManifest { prepared: self })
    }
}

/// A complete successor is visible; only explicit directory sync remains.
/// Retains old/new file locks. Drop never syncs or revokes the publication.
#[cfg(target_os = "linux")]
pub(crate) struct PublishedManifest {
    prepared: PreparedManifest,
}
#[cfg(target_os = "linux")]
impl PublishedManifest {
    pub(crate) fn sync(self, sync_parent: fn(&File) -> io::Result<()>) -> io::Result<()> {
        sync_parent(&self.prepared.locked.directory)
    }
}

#[cfg(target_os = "linux")]
impl Drop for PreparedManifest {
    fn drop(&mut self) {
        if let Some(name) = &self.temporary
            && let Ok(current) = statat(&self.locked.directory, name, AtFlags::SYMLINK_NOFOLLOW)
            && let Ok(retained) = self.file.metadata()
            && current.st_dev == retained.dev()
            && current.st_ino == retained.ino()
        {
            let _ = rustix::fs::unlinkat(&self.locked.directory, name, AtFlags::empty());
        }
    }
}
#[cfg(target_os = "linux")]
fn stale(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn parent_sync_failure_leaves_complete_successor_visible() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image");
        std::fs::write(&image, [37; 512]).unwrap();
        let graph = crate::ImageGraph::open(&[crate::ImageSpec {
            path: image.clone(),
            format: crate::ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        let old = graph.manifest(None).unwrap();
        let next = graph.manifest(Some(&image)).unwrap();
        let path = dir.path().join("manifest");
        old.save(&path).unwrap();
        let error = next
            .replace_inner(&path, &old, &mut OperationContext::default(), |_| {
                Err(io::Error::other("injected parent sync failure"))
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected parent sync failure"));
        assert_eq!(
            GraphManifest::open(&path).unwrap().selected(),
            Some(image.as_path())
        );
        assert_eq!(dir.path().read_dir().unwrap().count(), 2);
        assert!(next.replace(&path, &old).is_err());
    }
    #[test]
    fn preparation_retains_source_lock_and_abort_removes_only_staging() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image");
        std::fs::write(&image, [37; 512]).unwrap();
        let graph = crate::ImageGraph::open(&[crate::ImageSpec {
            path: image.clone(),
            format: crate::ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        let old = graph.manifest(None).unwrap();
        let next = graph.manifest(Some(&image)).unwrap();
        let path = dir.path().join("manifest");
        old.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let locked = LockedManifest::open(&path, &old).unwrap();
        let prepared = locked.prepare(&next).unwrap();
        let contender = std::fs::File::open(&path).unwrap();
        assert!(contender.try_lock().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(dir.path().read_dir().unwrap().count(), 3);
        drop(prepared);
        contender.try_lock().unwrap();
        assert_eq!(dir.path().read_dir().unwrap().count(), 2);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn prepared_publication_rechecks_source_and_keeps_foreign_staging() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image");
        std::fs::write(&image, [37; 512]).unwrap();
        let graph = crate::ImageGraph::open(&[crate::ImageSpec {
            path: image.clone(),
            format: crate::ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        let old = graph.manifest(None).unwrap();
        let next = graph.manifest(Some(&image)).unwrap();
        let path = dir.path().join("manifest");
        old.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let prepared = LockedManifest::open(&path, &old)
            .unwrap()
            .prepare(&next)
            .unwrap();
        let staged = dir
            .path()
            .read_dir()
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".virtdisk-manifest-")
            })
            .unwrap();
        std::fs::rename(&staged, dir.path().join("moved-stage")).unwrap();
        std::fs::write(&staged, b"foreign").unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(staged).unwrap(), b"foreign");
    }
    #[test]
    fn published_state_retains_successor_lock_until_explicit_sync() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("image");
        std::fs::write(&image, [37; 512]).unwrap();
        let graph = crate::ImageGraph::open(&[crate::ImageSpec {
            path: image.clone(),
            format: crate::ImageFormat::Raw,
            parent: None,
        }])
        .unwrap();
        let old = graph.manifest(None).unwrap();
        let next = graph.manifest(Some(&image)).unwrap();
        let path = dir.path().join("manifest");
        old.save(&path).unwrap();
        let published = LockedManifest::open(&path, &old)
            .unwrap()
            .prepare(&next)
            .unwrap()
            .publish()
            .unwrap();
        assert_eq!(
            GraphManifest::open(&path).unwrap().selected(),
            Some(image.as_path())
        );
        let contender = std::fs::File::open(&path).unwrap();
        assert!(contender.try_lock().is_err());
        published.sync(std::fs::File::sync_all).unwrap();
        contender.try_lock().unwrap();
        assert_eq!(dir.path().read_dir().unwrap().count(), 2);
    }
}
