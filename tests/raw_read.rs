#![cfg(feature = "std")]
use virtdisk::{RawDisk, ReadAt};

#[test]
fn regular_image_retains_canonical_context_and_bounded_reads() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    std::fs::write(&path, [1, 2, 3]).unwrap();
    let disk = RawDisk::open(&path).unwrap();
    let mut bytes = [0; 2];
    disk.read_exact_at(1, &mut bytes).unwrap();
    assert_eq!(bytes, [2, 3]);
    assert_eq!(
        disk.context().container.as_deref(),
        Some(path.canonicalize().unwrap().to_string_lossy().as_ref())
    );
    assert!(disk.read_exact_at(2, &mut bytes).is_err());
    assert!(RawDisk::open(directory.path()).is_err());
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.fifo");
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &path,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        let result = RawDisk::open(reader_path).map(|_| ()).map_err(|e| e.kind());
        send.send(result).unwrap();
    });
    let result = receive.recv_timeout(std::time::Duration::from_secs(2));
    // Unblock an accidentally blocking open before reporting a regression.
    let _connection = if result.is_err() {
        Some(
            rustix::fs::open(
                &path,
                rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .unwrap(),
        )
    } else {
        None
    };
    reader.join().unwrap();
    assert_eq!(result.unwrap(), Err(virtdisk::io::ErrorKind::InvalidInput));
}

#[cfg(unix)]
#[test]
fn regular_symlink_remains_supported() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.raw");
    let link = directory.path().join("link.raw");
    std::fs::write(&path, [7]).unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let disk = RawDisk::open(&link).unwrap();
    let mut bytes = [0];
    disk.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [7]);
    assert_eq!(
        disk.context().container,
        Some(path.to_string_lossy().into_owned())
    );
}
