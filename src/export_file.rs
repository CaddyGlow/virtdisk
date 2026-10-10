//! Optional component measurements around private export files.
use crate::{OperationEvent, OperationProfiler};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::ops::Deref;

#[cfg(test)]
std::thread_local! {
    static FAULT: std::cell::Cell<Option<(OperationEvent, usize)>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn fail_if_requested(event: OperationEvent) -> io::Result<()> {
    FAULT.with(|fault| {
        if let Some((target, remaining)) = fault.get()
            && event == target
        {
            if remaining == 0 {
                return Err(io::Error::from_raw_os_error(if cfg!(windows) {
                    112
                } else {
                    28
                }));
            }
            fault.set(Some((target, remaining - 1)));
        }
        Ok(())
    })
}
pub(crate) struct ExportFile {
    file: File,
    profiler: Option<OperationProfiler>,
    payload: bool,
    position: Option<u64>,
}
impl ExportFile {
    pub(crate) fn new(file: File, profiler: Option<OperationProfiler>) -> Self {
        Self {
            file,
            profiler,
            payload: false,
            position: Some(0),
        }
    }
    pub(crate) fn payload(&mut self, payload: bool) {
        self.payload = payload;
    }
    pub(crate) fn into_inner(self) -> File {
        self.file
    }
    pub(crate) fn sync_all(&self) -> io::Result<()> {
        let sync = || {
            #[cfg(test)]
            fail_if_requested(OperationEvent::Sync)?;
            self.file.sync_all()
        };
        if let Some(profiler) = &self.profiler {
            profiler.measure(OperationEvent::Sync, 0, sync)
        } else {
            sync()
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
        let mut write = || {
            #[cfg(test)]
            fail_if_requested(event)?;
            self.file.write(bytes)
        };
        let result = if let Some(profiler) = &self.profiler {
            profiler.measure(event, bytes.len() as u64, write)
        } else {
            write()
        };
        self.position = result.as_ref().ok().and_then(|count| {
            self.position
                .and_then(|position| position.checked_add(*count as u64))
        });
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}
impl Seek for ExportFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if let SeekFrom::Start(offset) = position
            && self.position == Some(offset)
        {
            return Ok(offset);
        }
        let result = if let Some(profiler) = &self.profiler {
            profiler.measure(OperationEvent::Seek, 0, || self.file.seek(position))
        } else {
            self.file.seek(position)
        };
        self.position = result.as_ref().ok().copied();
        result
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
    #[test]
    fn streaming_disk_full_at_payload_metadata_and_sync_keeps_output_unpublished() {
        struct Source;
        impl crate::ReadAt for Source {
            fn len(&self) -> u64 {
                1048576 + 512
            }
            fn read_exact_at(&self, _: u64, bytes: &mut [u8]) -> crate::io::Result<()> {
                bytes.fill(41);
                Ok(())
            }
        }
        for format in [crate::ImageFormat::Vdi, crate::ImageFormat::Vhdx] {
            for event in [
                OperationEvent::PayloadWrite,
                OperationEvent::MetadataWrite,
                OperationEvent::Sync,
            ] {
                for cut in [0, 1] {
                    let directory = tempfile::tempdir().unwrap();
                    let path = directory.path().join("out");
                    let profiler = OperationProfiler::default();
                    let limits = crate::OperationLimits::default()
                        .working_memory_bytes(2 << 20)
                        .unwrap();
                    let mut context = crate::OperationContext::new(limits)
                        .with_streaming_allocation(true)
                        .with_profiler(profiler.clone());
                    FAULT.with(|fault| fault.set(Some((event, cut))));
                    let result = crate::convert_image_with_policy_and_context(
                        &Source,
                        &path,
                        format,
                        crate::ConversionPolicy::Sparse,
                        &mut context,
                    );
                    FAULT.with(|fault| fault.set(None));
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.raw_os_error(),
                        Some(if cfg!(windows) { 112 } else { 28 })
                    );
                    assert!(!path.exists());
                    assert_eq!(profiler.metrics(event).failures, 1);
                    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
                }
            }
        }
    }

    #[test]
    fn adjacent_payload_writes_skip_redundant_seeks() {
        let profiler = OperationProfiler::default();
        let mut file = ExportFile::new(tempfile::tempfile().unwrap(), Some(profiler.clone()));
        file.payload(true);
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&[1; 4]).unwrap();
        file.seek(SeekFrom::Start(4)).unwrap();
        file.write_all(&[2; 4]).unwrap();
        assert_eq!(profiler.metrics(OperationEvent::Seek).calls, 0);
        file.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(profiler.metrics(OperationEvent::Seek).calls, 1);
    }
}
