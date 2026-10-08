//! Explicit policies for opening a mutable image.
use std::{
    fmt, io,
    path::{Path, PathBuf},
};

/// Whether opening may replay validated pending transaction metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RecoveryPolicy {
    /// Refuse pending recovery without changing image bytes or evidence.
    #[default]
    RejectPending,
    /// Replay supported recovery after acquiring locks and validating dependencies.
    /// Recovery failure can leave partially replayed metadata; retry before I/O.
    Recover,
}
impl RecoveryPolicy {
    pub(crate) fn check(self, pending: bool) -> io::Result<()> {
        if pending && self == Self::RejectPending {
            Err(io::Error::new(io::ErrorKind::InvalidData, RecoveryRequired))
        } else {
            Ok(())
        }
    }
    pub(crate) fn check_sidecar(self, path: &Path) -> io::Result<()> {
        let pending = match std::fs::symlink_metadata(path) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        self.check(pending)
    }
}

/// Typed refusal when the caller has not authorized pending recovery.
/// Access through `io::Error::get_ref()`; no replay or cleanup has occurred.
#[derive(Debug)]
pub struct RecoveryRequired;
impl fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("image requires explicitly authorized recovery")
    }
}
impl std::error::Error for RecoveryRequired {}

/// Explicit opening policy for [`crate::ImageWriter::open_with_options`].
/// Defaults to standalone opening with recovery forbidden.
#[derive(Debug, Clone, Default)]
pub struct WriterOpenOptions {
    pub(crate) authorized: Option<Vec<PathBuf>>,
    pub(crate) recovery: RecoveryPolicy,
}
impl WriterOpenOptions {
    /// Select whether validated pending recovery may modify the image.
    #[must_use]
    pub fn recovery_policy(mut self, policy: RecoveryPolicy) -> Self {
        self.recovery = policy;
        self
    }
    /// Authorize exactly these parent/extent paths. VDI paths are ordered from
    /// direct parent to base. An empty list explicitly selects chain resolution
    /// without authorizing any embedded dependency.
    #[must_use]
    pub fn authorized_paths(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.authorized = Some(paths.into_iter().collect());
        self
    }
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn refuse_pending_open(
    path: &std::path::Path,
    format: crate::ImageFormat,
    authorized: Option<&[std::path::PathBuf]>,
) {
    let snapshot = || {
        let mut files = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    };
    let before = snapshot();
    let mut options = crate::WriterOpenOptions::default();
    if let Some(paths) = authorized {
        options = options.authorized_paths(paths.iter().cloned());
    }
    let error = crate::ImageWriter::open_with_options(path, format, &options)
        .err()
        .unwrap();
    assert!(error.get_ref().unwrap().is::<crate::RecoveryRequired>());
    assert_eq!(snapshot(), before);
}
