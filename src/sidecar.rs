//! Bounded reads of transaction sidecars, without following links or blocking on FIFOs.
use crate::io::{self, Read};
use std::path::Path;

/// Failure after the journal name became visible. The caller must retain its
/// recovery state even if a later replay removes that name.
#[derive(Debug)]
pub(crate) struct PublishedError(io::Error);

impl core::fmt::Display for PublishedError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for PublishedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub(crate) fn publication_requires_reopen(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<PublishedError>())
}

pub(crate) fn published_error(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), PublishedError(error))
}

pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Ok(std::fs::File::open(parent)?.sync_all()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "journal directory durability currently requires Linux",
        ))
    }
}

struct TemporaryPath {
    path: std::path::PathBuf,
    owned: bool,
}

impl TemporaryPath {
    fn create(path: &Path) -> io::Result<(Self, std::fs::File)> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        // A crashed process can leave temporary names behind. Skip collisions
        // rather than preventing publication after a process ID is reused.
        for _ in 0..128 {
            let mut name = path.as_os_str().to_owned();
            name.push(format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let name = std::path::PathBuf::from(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&name)
            {
                Ok(file) => {
                    return Ok((
                        Self {
                            path: name,
                            owned: true,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "journal temporary name collision limit exceeded",
        ))
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Publish a synced sidecar without replacing existing recovery evidence.
/// Errors after linking carry `PublishedError`; only the owned temporary name
/// is cleaned up on failure.
pub(crate) fn publish_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish_new_with_sync(path, bytes, sync_parent)
}

fn publish_new_with_sync(
    path: &Path,
    bytes: &[u8],
    sync: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<()> {
    use io::Write;
    let (mut temporary, file) = TemporaryPath::create(path)?;
    // Close the handle before unlinking its temporary name on every path.
    (|| {
        let mut file = file;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok::<_, io::Error>(())
    })()?;
    std::fs::hard_link(&temporary.path, path)?;
    (|| {
        sync(path)?;
        std::fs::remove_file(&temporary.path)?;
        temporary.owned = false;
        sync(path)
    })()
    .map_err(published_error)
}

pub(crate) fn read_bounded(
    path: &Path,
    limit: usize,
    invalid: fn() -> io::Error,
) -> io::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::OFlags;
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid());
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn invalid() -> io::Error {
        io::ErrorKind::InvalidData.into()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publication_preserves_existing_evidence_and_cleans_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        publish_new(&path, b"original").unwrap();
        let error = publish_new(&path, b"replacement").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(!publication_requires_reopen(&error));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_directory_sync_retains_published_journal_and_typed_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        let error = publish_new_with_sync(&path, b"recovery evidence", |_| {
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull).into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        assert!(publication_requires_reopen(&error));
        assert_eq!(std::fs::read(&path).unwrap(), b"recovery evidence");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let cause = error.get_ref().unwrap().source().unwrap();
        let error = cause.downcast_ref::<io::Error>().unwrap();
        assert!(error.get_ref().unwrap().is::<std::io::Error>());
    }
    #[test]
    fn rejects_oversized_files_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        assert!(read_bounded(&path, 4, invalid).unwrap().is_none());
        std::fs::write(&path, b"1234").unwrap();
        assert_eq!(read_bounded(&path, 4, invalid).unwrap().unwrap(), b"1234");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_bounded(&path, 4, invalid).is_err());
        #[cfg(target_os = "linux")]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_bounded(&link, 4, invalid).is_err());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_fifo_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal");
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert_eq!(
            read_bounded(&path, 4, invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
