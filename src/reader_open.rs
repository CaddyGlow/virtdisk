//! Explicit immutable reader opening policies.
use crate::io;
use crate::{ImageFormat, ParserLimits};
use std::path::PathBuf;

/// Recovery allowed while opening an immutable reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadRecoveryPolicy {
    /// Reject pending journals/logs through the selected format's normal parser.
    #[default]
    RejectPending,
    /// Validate and overlay a VHDX native log in memory without modifying files.
    /// Other formats reject this policy rather than ignoring it.
    ReplayVhdxLog,
}
/// Explicit format, dependency authorization and caller-tightened reader limits.
/// Defaults to automatic binary recognition, standalone opening and no recovery.
#[derive(Debug, Clone, Default)]
pub struct ReaderOpenOptions {
    pub(crate) format: Option<ImageFormat>,
    pub(crate) authorized: Option<Vec<PathBuf>>,
    pub(crate) limits: ParserLimits,
    pub(crate) recovery: ReadRecoveryPolicy,
}
impl ReaderOpenOptions {
    /// Validated parser ceilings retained by readers opened with these options.
    pub fn limits(&self) -> ParserLimits {
        self.limits
    }
    /// Immutable recovery policy selected by the caller.
    pub fn recovery(&self) -> ReadRecoveryPolicy {
        self.recovery
    }
    /// Select an explicit format. Raw bypasses binary recognition deliberately.
    #[must_use]
    pub fn format(mut self, format: ImageFormat) -> Self {
        self.format = Some(format);
        self
    }
    /// Authorize only these parent/extent paths; VDI ancestors are ordered from
    /// direct parent to base. An empty list selects chain resolution without
    /// authorizing dependencies. Raw rejects a nonempty list.
    #[must_use]
    pub fn authorized_paths(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.authorized = Some(paths.into_iter().collect());
        self
    }
    /// Validate caller ceilings before any image is opened. Limits may only
    /// tighten hard defaults and remain attached to deferred reads and parents.
    pub fn parser_limits(mut self, limits: ParserLimits) -> io::Result<Self> {
        limits.validate()?;
        self.limits = limits;
        Ok(self)
    }
    /// Select immutable recovery; this never authorizes native file mutation.
    #[must_use]
    pub fn recovery_policy(mut self, policy: ReadRecoveryPolicy) -> Self {
        self.recovery = policy;
        self
    }
}
