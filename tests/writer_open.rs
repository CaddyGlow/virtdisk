use std::{fs, io};
use virtdisk::{ImageFormat, ImageWriter, RecoveryPolicy, WriteAt, WriterOpenOptions};

#[test]
fn explicit_clean_open_preserves_pending_evidence_and_image() {
    let directory = tempfile::tempdir().unwrap();
    for (format, suffix) in [
        (ImageFormat::Qcow2, ".virtdisk-qcow2-journal"),
        (ImageFormat::Vdi, ".virtdisk-transaction"),
        (ImageFormat::Vmdk, ".virtdisk-transaction"),
    ] {
        let path = directory.path().join(format!("{format:?}"));
        let writer = ImageWriter::create(&path, format, 65536).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let original = fs::read(&path).unwrap();
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        fs::write(&sidecar, b"pending evidence").unwrap();
        let error = ImageWriter::open_with_options(&path, format, &WriterOpenOptions::default())
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.get_ref().unwrap().is::<virtdisk::RecoveryRequired>());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read(&sidecar).unwrap(), b"pending evidence");
    }
}

#[test]
fn explicit_recovery_opens_clean_profiles_and_retains_exclusive_lock() {
    let directory = tempfile::tempdir().unwrap();
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vhdx,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
    ] {
        let path = directory.path().join(format!("{format:?}"));
        drop(ImageWriter::create(&path, format, 65536).unwrap());
        let options = WriterOpenOptions::default().recovery_policy(RecoveryPolicy::Recover);
        let writer = ImageWriter::open_with_options(&path, format, &options).unwrap();
        assert!(ImageWriter::open_with_options(&path, format, &options).is_err());
        writer.write_all_at(0, &[17; 512]).unwrap();
        writer.flush().unwrap();
        let mut bytes = [0; 512];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, [17; 512]);
    }
}

#[test]
fn raw_open_options_refuse_dependencies_instead_of_ignoring_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    fs::write(&path, [37; 512]).unwrap();
    let options =
        WriterOpenOptions::default().authorized_paths([directory.path().join("absent-parent")]);
    let error = ImageWriter::open_with_options(&path, ImageFormat::Raw, &options)
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(fs::read(&path).unwrap(), [37; 512]);
    let empty = WriterOpenOptions::default().authorized_paths([]);
    assert!(ImageWriter::open_with_options(&path, ImageFormat::Raw, &empty).is_ok());
}
