//! Fixed-length adapters used while a caller retains a writer's operation lock.
use crate::io::{self, Read, Seek, SeekFrom};
use crate::{RawWriter, ReadAt, check_range};
use std::{
    fs::File,
    sync::{Arc, Mutex},
};

pub(crate) struct FileSource {
    file: Mutex<File>,
    size: u64,
}
impl FileSource {
    pub(crate) fn new(file: File, size: u64) -> Self {
        Self {
            file: Mutex::new(file),
            size,
        }
    }
}
impl ReadAt for FileSource {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        check_range(offset, dst.len() as u64, self.size)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("VHDX source mutex poisoned"))?;
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.read_exact(dst)?)
    }
}

pub(crate) struct LockedSource {
    pub(crate) raw: Arc<RawWriter>,
    pub(crate) size: u64,
}
impl ReadAt for LockedSource {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        check_range(offset, dst.len() as u64, self.size)?;
        self.raw.read_exact_at(offset, dst)
    }
}

pub(crate) struct ZeroSource(pub(crate) u64);
impl ReadAt for ZeroSource {
    fn len(&self) -> u64 {
        self.0
    }
    fn read_exact_at(&self, offset: u64, dst: &mut [u8]) -> io::Result<()> {
        check_range(offset, dst.len() as u64, self.0)?;
        dst.fill(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_sources_keep_their_original_length_after_writer_growth() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("raw");
        let raw = Arc::new(RawWriter::create(&path, 4).unwrap());
        raw.write_all_at(0, &[1, 2, 3, 4]).unwrap();
        let retained = LockedSource {
            raw: raw.clone(),
            size: 4,
        };
        let file = FileSource::new(File::open(&path).unwrap(), 4);
        raw.resize(8).unwrap();
        raw.write_all_at(4, &[5, 6, 7, 8]).unwrap();
        for source in [&retained as &dyn ReadAt, &file, &ZeroSource(4)] {
            assert_eq!(source.len(), 4);
            assert!(source.read_exact_at(4, &mut []).is_ok());
            assert_eq!(
                source.read_exact_at(4, &mut [0]).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            assert_eq!(
                source.read_exact_at(u64::MAX, &mut [0]).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
        let mut bytes = [0; 4];
        retained.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4]);
        // FileSource production callers clone the lock-owning file handle.
        // This fixture independently reopens the path, so on Windows it cannot
        // read the locked range until every retained writer handle is dropped.
        drop(retained);
        drop(raw);
        file.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4]);
    }
}
