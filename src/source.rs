//! Fixed-length adapters used while a caller retains a writer's operation lock.
use crate::io;
use crate::{RawWriter, ReadAt, check_range};
use std::{fs::File, sync::Arc};
#[cfg(not(unix))]
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};

/// Retains one opened handle; only the portable fallback owns a mutable cursor.
pub(crate) struct ReadFile {
    #[cfg(unix)]
    file: File,
    #[cfg(not(unix))]
    file: Mutex<File>,
}
impl ReadFile {
    pub(crate) fn new(file: File) -> Self {
        Self {
            #[cfg(unix)]
            file,
            #[cfg(not(unix))]
            file: Mutex::new(file),
        }
    }
    pub(crate) fn with_file<T>(
        &self,
        operation: impl FnOnce(&File) -> io::Result<T>,
    ) -> io::Result<T> {
        #[cfg(unix)]
        {
            operation(&self.file)
        }
        #[cfg(not(unix))]
        {
            let file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("source mutex poisoned"))?;
            operation(&file)
        }
    }
    pub(crate) fn read_exact_at(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            positioned_exact(offset, destination, |offset, destination| {
                self.file.read_at(destination, offset)
            })
        }
        #[cfg(not(unix))]
        {
            let mut file = self
                .file
                .lock()
                .map_err(|_| io::Error::other("source mutex poisoned"))?;
            file.seek(SeekFrom::Start(offset))?;
            Ok(file.read_exact(destination)?)
        }
    }
}

#[cfg(any(unix, test))]
fn positioned_exact(
    mut offset: u64,
    mut destination: &mut [u8],
    mut read: impl FnMut(u64, &mut [u8]) -> std::io::Result<usize>,
) -> io::Result<()> {
    offset
        .checked_add(destination.len() as u64)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "source read range overflow")
        })?;
    while !destination.is_empty() {
        match read(offset, destination) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source file shortened during read",
                ));
            }
            Ok(count) => {
                offset += count as u64;
                destination = &mut destination[count..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) struct FileSource {
    file: ReadFile,
    size: u64,
}
impl FileSource {
    pub(crate) fn new(file: File, size: u64) -> Self {
        Self {
            file: ReadFile::new(file),
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
        self.file.read_exact_at(offset, dst)
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
    fn positioned_reads_retry_interruptions_and_short_reads() {
        let mut calls = 0;
        let mut output = [0; 5];
        positioned_exact(7, &mut output, |offset, bytes| {
            calls += 1;
            if calls == 1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(2);
            for (index, byte) in bytes[..count].iter_mut().enumerate() {
                *byte = (offset + index as u64) as u8;
            }
            Ok(count)
        })
        .unwrap();
        assert_eq!(output, [7, 8, 9, 10, 11]);
        assert_eq!(calls, 4);
    }

    #[test]
    fn positioned_reads_preserve_eof_errors_and_reject_overflow_before_io() {
        assert_eq!(
            positioned_exact(0, &mut [0], |_, _| Ok(0))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            positioned_exact(0, &mut [0], |_, _| Err(
                std::io::ErrorKind::PermissionDenied.into()
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );
        positioned_exact(u64::MAX, &mut [], |_, _| {
            panic!("empty reads do not issue I/O")
        })
        .unwrap();
        assert_eq!(
            positioned_exact(u64::MAX, &mut [0], |_, _| panic!(
                "overflow must precede I/O"
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[cfg(unix)]
    #[test]
    fn positioned_read_keeps_cloned_cursor_and_supports_shared_readers() {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&(0..=255).collect::<Vec<u8>>()).unwrap();
        file.seek(SeekFrom::Start(31)).unwrap();
        let mut cursor = file.try_clone().unwrap();
        let source = FileSource::new(file, 256);
        std::thread::scope(|scope| {
            for offset in 0..8 {
                let source = &source;
                scope.spawn(move || {
                    for _ in 0..100 {
                        let mut bytes = [0; 32];
                        source.read_exact_at(offset, &mut bytes).unwrap();
                        assert_eq!(bytes[0], offset as u8);
                        assert_eq!(bytes[31], offset as u8 + 31);
                    }
                });
            }
        });
        assert_eq!(cursor.stream_position().unwrap(), 31);
        cursor.set_len(8).unwrap();
        assert_eq!(
            source.read_exact_at(0, &mut [0; 16]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

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
