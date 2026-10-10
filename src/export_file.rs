//! Optional component measurements around private export files.
use crate::{OperationEvent, OperationProfiler};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::Deref;

pub(crate) struct ExportFile {
    file: File,
    profiler: Option<OperationProfiler>,
    payload: bool,
}
impl ExportFile {
    pub(crate) fn new(file: File, profiler: Option<OperationProfiler>) -> Self {
        Self {
            file,
            profiler,
            payload: false,
        }
    }
    pub(crate) fn payload(&mut self, payload: bool) {
        self.payload = payload;
    }
    pub(crate) fn into_inner(self) -> File {
        self.file
    }
    pub(crate) fn sync_all(&self) -> io::Result<()> {
        if let Some(profiler) = &self.profiler {
            profiler.measure(OperationEvent::Sync, 0, || self.file.sync_all())
        } else {
            self.file.sync_all()
        }
    }
}
impl Deref for ExportFile {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl Write for ExportFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let event = if self.payload {
            OperationEvent::PayloadWrite
        } else {
            OperationEvent::MetadataWrite
        };
        if let Some(profiler) = &self.profiler {
            profiler.measure(event, bytes.len() as u64, || self.file.write(bytes))
        } else {
            self.file.write(bytes)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for ExportFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if let Some(profiler) = &self.profiler {
            profiler.measure(OperationEvent::Seek, 0, || self.file.seek(position))
        } else {
            self.file.seek(position)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_payload_write_preserves_error_and_records_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readonly");
        std::fs::write(&path, []).unwrap();
        let profiler = OperationProfiler::default();
        let mut file = ExportFile::new(File::open(path).unwrap(), Some(profiler.clone()));
        file.payload(true);
        let error = file.write_all(&[1, 2, 3]).unwrap_err();
        assert!(error.raw_os_error().is_some());
        let metrics = profiler.metrics(OperationEvent::PayloadWrite);
        assert_eq!(metrics.calls, 1);
        assert_eq!(metrics.bytes, 3);
        assert_eq!(metrics.failures, 1);
        assert_eq!(profiler.metrics(OperationEvent::MetadataWrite).calls, 0);
    }
}
