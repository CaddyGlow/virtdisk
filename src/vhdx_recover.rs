//! Explicit locked native redo recovery for standalone VHDX images.
use crate::source::FileSource;
use crate::{ParserLimits, Vhdx};
#[cfg(test)]
use crate::{ReadAt, check_range};
use std::{
    fs::{File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Validated,
    EpochFirstSynced,
    EpochSecondSynced,
    PatchApplied(usize),
    ReplaySynced,
    ClearFirstSynced,
    ClearSecondSynced,
}
/// Replay a standalone VHDX's native log into its exclusively locked backing file.
///
/// The active log and the complete recovered image are validated before any
/// mutation. Fresh file/data UUID epochs are durably installed before replay.
/// Original log entries remain untouched throughout idempotent redo. Replayed
/// structures and virtual extension are synced before the log GUID is cleared
/// through redundant headers. Clean images are validated and left unchanged.
/// Uses the default bounded parser budget; oversized logs or unsupported
/// differencing images fail before mutation. An error may leave a partially
/// replayed image: retry recovery before normal I/O. This has no rollback and
/// requires exclusion of non-cooperating external writers. Host fault tests do
/// not establish native Windows power-loss correctness.
pub fn recover_vhdx(path: impl AsRef<Path>) -> io::Result<()> {
    recover_with_hook(path, |_| Ok(()))
}
/// Recover a child under exclusive lock, validating its explicitly authorized clean parent chain first.
/// Parents remain unchanged. Original child native redo remains available after interrupted recovery.
pub fn recover_vhdx_chain(
    path: impl AsRef<Path>,
    authorized_parent_paths: &[std::path::PathBuf],
) -> io::Result<()> {
    recover_impl(path, Some(authorized_parent_paths), |_| Ok(()))
}
fn install(file: &mut File, header: &mut [u8; 4096], offset: u64, sequence: u64) -> io::Result<()> {
    header[8..16].copy_from_slice(&sequence.to_le_bytes());
    crate::vhdx_write::checksum(header);
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(header)?;
    file.sync_all()
}
fn recover_with_hook(
    path: impl AsRef<Path>,
    hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    recover_impl(path, None, hook)
}
fn recover_impl(
    path: impl AsRef<Path>,
    authorized: Option<&[std::path::PathBuf]>,
    hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let path = path.as_ref();
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VHDX recovery requires a regular file",
        ));
    }
    file.try_lock().map_err(io::Error::from)?;
    recover_locked_with_hook(file, path, authorized, crate::RecoveryPolicy::Recover, hook).map(drop)
}
pub(crate) fn recover_locked(
    file: File,
    path: &Path,
    authorized: Option<&[std::path::PathBuf]>,
    policy: crate::RecoveryPolicy,
) -> io::Result<File> {
    recover_locked_with_hook(file, path, authorized, policy, |_| Ok(()))
}
fn recover_locked_with_hook(
    mut file: File,
    path: &Path,
    authorized: Option<&[std::path::PathBuf]>,
    policy: crate::RecoveryPolicy,
    mut hook: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<File> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "VHDX recovery requires a regular file",
        ));
    }
    let source = Arc::new(FileSource::new(file.try_clone()?, meta.len()));
    let (_image, overlay) = if let Some(approved) = authorized {
        Vhdx::recovered_chain_parts(
            source,
            path,
            approved,
            same_file::Handle::from_file(file.try_clone()?)?,
            ParserLimits::default(),
        )?
    } else {
        Vhdx::recovered_parts(source, ParserLimits::default())?
    };
    overlay.reserve_replay_work()?;
    hook(Stage::Validated)?;
    let Some((active, mut header)) = overlay.native_header() else {
        return Ok(file);
    };
    policy.check(true)?;
    let sequence = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let final_sequence = sequence.checked_add(4).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "VHDX recovery header sequence exhausted",
        )
    })?;
    header[16..32].copy_from_slice(&crate::vhdx_write::identity()?);
    header[32..48].copy_from_slice(&crate::vhdx_write::identity()?);
    let inactive = if active == 65536 { 131072 } else { 65536 };
    install(&mut file, &mut header, inactive, sequence + 1)?;
    hook(Stage::EpochFirstSynced)?;
    install(&mut file, &mut header, active, sequence + 2)?;
    hook(Stage::EpochSecondSynced)?;
    overlay.replay_to(&mut file, |index| hook(Stage::PatchApplied(index)))?;
    file.sync_all()?;
    hook(Stage::ReplaySynced)?;
    header[48..64].fill(0);
    install(&mut file, &mut header, inactive, sequence + 3)?;
    hook(Stage::ClearFirstSynced)?;
    install(&mut file, &mut header, active, final_sequence)?;
    hook(Stage::ClearSecondSynced)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    const M: usize = 1 << 20;
    struct Filled;
    impl ReadAt for Filled {
        fn len(&self) -> u64 {
            M as u64
        }
        fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
            check_range(offset, out.len() as u64, M as u64)?;
            out.fill(17);
            Ok(())
        }
    }
    fn dirty(path: &Path) {
        crate::create_vhdx(path, &Filled).unwrap();
        let mut b = std::fs::read(path).unwrap();
        let e = &mut b[M..M + 4096];
        e[..4].copy_from_slice(b"loge");
        e[8..12].copy_from_slice(&4096u32.to_le_bytes());
        e[16..24].copy_from_slice(&1u64.to_le_bytes());
        e[24..28].copy_from_slice(&1u32.to_le_bytes());
        e[32..48].fill(0x55);
        e[48..56].copy_from_slice(&(5 * M as u64).to_le_bytes());
        e[56..64].copy_from_slice(&(5 * M as u64).to_le_bytes());
        e[64..68].copy_from_slice(b"zero");
        e[72..80].copy_from_slice(&4096u64.to_le_bytes());
        e[80..88].copy_from_slice(&(2 * M as u64).to_le_bytes());
        e[88..96].copy_from_slice(&1u64.to_le_bytes());
        crate::vhdx_write::checksum(e);
        for offset in [65536, 131072] {
            b[offset + 48..offset + 64].fill(0x55);
            crate::vhdx_write::checksum(&mut b[offset..offset + 4096]);
        }
        b[2 * M..2 * M + 4096].fill(0xff);
        std::fs::write(path, b).unwrap();
    }
    #[test]
    fn interruption_at_each_native_persistence_boundary_is_recoverable() {
        let _process_boundary = crate::test_sync::writer_test();
        for stop in [
            Stage::Validated,
            Stage::EpochFirstSynced,
            Stage::EpochSecondSynced,
            Stage::PatchApplied(0),
            Stage::ReplaySynced,
            Stage::ClearFirstSynced,
            Stage::ClearSecondSynced,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk");
            dirty(&path);
            let original = std::fs::read(&path).unwrap();
            let result = recover_with_hook(&path, |stage| {
                if stage == stop {
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "injected persistence interruption",
                    ))
                } else {
                    Ok(())
                }
            });
            assert_eq!(
                result.unwrap_err().kind(),
                io::ErrorKind::Interrupted,
                "{stop:?}"
            );
            assert_eq!(
                &std::fs::read(&path).unwrap()[M..2 * M],
                &original[M..2 * M]
            );
            if stop == Stage::Validated {
                assert_eq!(std::fs::read(&path).unwrap(), original);
            }
            let disk =
                Vhdx::open_recovered(Arc::new(crate::RawDisk::open(&path).unwrap())).unwrap();
            let mut out = [1; 512];
            disk.read_exact_at(0, &mut out).unwrap();
            assert_eq!(out, [0; 512], "{stop:?}");
            drop(disk);
            recover_vhdx(&path).unwrap();
            Vhdx::open(Arc::new(crate::RawDisk::open(path).unwrap())).unwrap();
        }
    }
    #[test]
    fn torn_new_header_and_partial_metadata_redo_preserve_recoverability() {
        let _process_boundary = crate::test_sync::writer_test();
        for stop in [
            Stage::EpochFirstSynced,
            Stage::PatchApplied(0),
            Stage::ClearFirstSynced,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disk");
            dirty(&path);
            let result = recover_with_hook(&path, |stage| {
                if stage != stop {
                    return Ok(());
                }
                let mut torn = OpenOptions::new().write(true).open(&path)?;
                if stage == Stage::PatchApplied(0) {
                    torn.seek(SeekFrom::Start((2 * M + 2048) as u64))?;
                    torn.write_all(&[0xff; 2048])?;
                } else {
                    // The newly installed inactive header is torn; the previous
                    // current header and the retained redo log remain valid.
                    torn.seek(SeekFrom::Start(65536 + 4))?;
                    torn.write_all(&[0; 4])?;
                }
                torn.sync_all()?;
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected torn write",
                ))
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            recover_vhdx(&path).unwrap();
            let disk = Vhdx::open(Arc::new(crate::RawDisk::open(path).unwrap())).unwrap();
            let mut data = [1; 512];
            disk.read_exact_at(0, &mut data).unwrap();
            assert_eq!(data, [0; 512], "{stop:?}");
        }
    }
}
