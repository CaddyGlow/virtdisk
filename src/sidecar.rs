//! Bounded reads of transaction sidecars, without following links or blocking on FIFOs.
use std::{
    io::{self, Read},
    path::Path,
};

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
        Err(error) => return Err(error),
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
