use crate::{ReadAt, ReadContext, check_range, io, source::ReadFile};
use std::{fs::File, path::Path};

/// A bounded, read-only Linux block device or Windows physical disk.
///
/// Capacity and logical sector size are captured from the opened device. Reads
/// may use whole sectors internally. This handle provides no snapshot or lock
/// against external writers: callers must keep the device immutable, including
/// its capacity, until all reads finish. No host-file identity is advertised.
pub struct PhysicalDisk {
    file: ReadFile,
    length: u64,
    sector_size: u32,
    context: ReadContext,
}

impl PhysicalDisk {
    /// Open a device read-only. Regular files and non-disk devices are rejected.
    /// Windows accepts only `\\.\PhysicalDriveN` paths; administrator access is
    /// generally required. Linux accepts block devices, including partitions.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let (file, length, sector_size) = open_device(path)?;
        if !(512..=65536).contains(&sector_size)
            || !sector_size.is_power_of_two()
            || length == 0
            || length > i64::MAX as u64
            || !length.is_multiple_of(u64::from(sector_size))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid device geometry",
            ));
        }
        Ok(Self {
            file: ReadFile::new(file),
            length,
            sector_size,
            context: ReadContext {
                container: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        })
    }

    /// Logical sector size reported by the device, in bytes.
    pub fn logical_sector_size(&self) -> u32 {
        self.sector_size
    }
}

impl ReadAt for PhysicalDisk {
    fn len(&self) -> u64 {
        self.length
    }
    fn context(&self) -> ReadContext {
        self.context.clone()
    }
    fn read_exact_at(&self, mut offset: u64, destination: &mut [u8]) -> io::Result<()> {
        let start = offset;
        let result: io::Result<()> = (|| {
            let mut destination = destination;
            check_range(offset, destination.len() as u64, self.length)?;
            if destination.is_empty() {
                return Ok(());
            }
            // A bounded bounce buffer also handles unaligned byte requests on
            // Windows disk handles without issuing unaligned device reads.
            let mut buffer = vec![0; 65536];
            while !destination.is_empty() {
                let within = (offset % u64::from(self.sector_size)) as usize;
                let count = destination.len().min(buffer.len() - within);
                let native_count = (within + count).next_multiple_of(self.sector_size as usize);
                self.file
                    .read_exact_at(offset - within as u64, &mut buffer[..native_count])?;
                destination[..count].copy_from_slice(&buffer[within..within + count]);
                destination = &mut destination[count..];
                offset += count as u64;
            }
            Ok(())
        })();
        result.map_err(|error| {
            let mut context = self.context();
            context.offset = Some(start);
            context.error("read physical disk", error)
        })
    }
}

#[cfg(target_os = "linux")]
fn open_device(path: &Path) -> io::Result<(File, u64, u32)> {
    use rustix::{
        fs::{Mode, OFlags, open},
        ioctl::{Getter, ioctl, opcode},
    };
    use std::os::unix::fs::FileTypeExt;
    let file: File = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?
    .into();
    if !file.metadata()?.file_type().is_block_device() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "physical disk must be a block device",
        ));
    }
    // SAFETY: Linux BLKGETSIZE64 writes a u64; its encoded size is sizeof(size_t).
    let length = unsafe {
        ioctl(
            &file,
            Getter::<{ opcode::read::<usize>(0x12, 114) }, u64>::new(),
        )
    }?;
    // SAFETY: Linux BLKSSZGET is _IO(0x12, 104) and writes an int.
    // The direction bits in _IO vary by architecture.
    let sector = unsafe { ioctl(&file, Getter::<{ opcode::none(0x12, 104) }, u32>::new()) }?;
    Ok((file, length, sector))
}

#[cfg(windows)]
fn open_device(path: &Path) -> io::Result<(File, u64, u32)> {
    use std::{
        fs::OpenOptions,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    };
    let name = path.as_os_str().to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "invalid physical drive path")
    })?;
    let number = name.get(17..).unwrap_or_default();
    if !name
        .get(..17)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\.\PhysicalDrive"))
        || number.is_empty()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected \\\\.\\PhysicalDriveN",
        ));
    }
    let file = OpenOptions::new().read(true).share_mode(3).open(path)?;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            handle: *mut core::ffi::c_void,
            code: u32,
            input: *const core::ffi::c_void,
            input_len: u32,
            output: *mut core::ffi::c_void,
            output_len: u32,
            returned: *mut u32,
            overlapped: *mut core::ffi::c_void,
        ) -> i32;
    }
    let query = |code, output: &mut [u8]| -> io::Result<()> {
        let mut returned = 0;
        // SAFETY: synchronous handle, valid writable output and byte-count
        // buffers, no input or OVERLAPPED structure. All pointers live through
        // the call. Output is decoded as bytes to avoid struct alignment issues.
        let success = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                code,
                core::ptr::null(),
                0,
                output.as_mut_ptr().cast(),
                output.len() as u32,
                &mut returned,
                core::ptr::null_mut(),
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if returned < output.len() as u32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short device geometry response",
            ));
        }
        Ok(())
    };
    let mut length = [0; 8];
    query(0x7405c, &mut length)?; // IOCTL_DISK_GET_LENGTH_INFO
    let mut geometry = [0; 24];
    query(0x70000, &mut geometry)?; // IOCTL_DISK_GET_DRIVE_GEOMETRY
    let sector = u32::from_le_bytes([geometry[20], geometry[21], geometry[22], geometry[23]]);
    Ok((file, u64::from_le_bytes(length), sector))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sector_bounce_reads_cover_boundaries_and_leave_invalid_buffers_untouched() {
        for sector_size in [512, 4096, 65536] {
            // Mix the full 512-byte sector number into each eight-byte group,
            // so a read from another sector cannot satisfy the comparison.
            let bytes: Vec<_> = (0..131072u64)
                .map(|offset| offset as u8 ^ ((offset >> 9) >> (8 * (offset % 8))) as u8)
                .collect();
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), &bytes).unwrap();
            let disk = PhysicalDisk {
                file: ReadFile::new(File::open(file.path()).unwrap()),
                length: bytes.len() as u64,
                sector_size,
                context: ReadContext::default(),
            };
            for (offset, length) in [(1, 100001), (65535, 65537), (131071, 1), (131072, 0)] {
                let mut actual = vec![0; length];
                disk.read_exact_at(offset as u64, &mut actual).unwrap();
                assert_eq!(actual, bytes[offset..offset + length]);
            }
            let mut invalid = [0xa5; 2];
            assert!(disk.read_exact_at(131071, &mut invalid).is_err());
            assert_eq!(invalid, [0xa5; 2]);
        }
    }
}
