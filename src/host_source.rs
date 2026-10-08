//! Retained host identity and native path information.
use crate::{SourceIdentity, io};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
};

/// Host provenance retained separately from portable diagnostic context.
pub struct HostSourceContext {
    /// Native path used by host recovery checks; never reconstructed from a label.
    pub path: PathBuf,
    identity: Arc<StorageIdentity>,
}
struct StorageIdentity {
    handle: same_file::Handle,
    namespace: u128,
    storage: u128,
}
struct Registry {
    namespace: Option<u128>,
    next: u128,
    sources: Vec<Weak<StorageIdentity>>,
}
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    namespace: None,
    next: 0,
    sources: Vec::new(),
});
impl HostSourceContext {
    pub(crate) fn new(path: PathBuf, file: &std::fs::File) -> io::Result<Self> {
        let handle = same_file::Handle::from_file(file.try_clone()?)?;
        let mut registry = REGISTRY
            .lock()
            .map_err(|_| io::Error::other("host identity registry poisoned"))?;
        registry.sources.retain(|source| source.strong_count() != 0);
        for source in &registry.sources {
            if let Some(identity) = source.upgrade()
                && identity.handle == handle
            {
                return Ok(Self { path, identity });
            }
        }
        let namespace = match registry.namespace {
            Some(namespace) => namespace,
            None => {
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
                let namespace = u128::from_le_bytes(bytes);
                registry.namespace = Some(namespace);
                namespace
            }
        };
        registry.next = registry
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("host identity registry exhausted"))?;
        let identity = Arc::new(StorageIdentity {
            handle,
            namespace,
            storage: registry.next,
        });
        registry.sources.try_reserve(1).map_err(|_| {
            io::Error::new(
                io::ErrorKind::OutOfMemory,
                "host identity registry allocation failed",
            )
        })?;
        registry.sources.push(Arc::downgrade(&identity));
        Ok(Self { path, identity })
    }
    pub(crate) fn token(&self, length: u64) -> SourceIdentity {
        SourceIdentity::new(self.identity.namespace, self.identity.storage, length)
    }
}
