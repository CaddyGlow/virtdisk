use std::{io, sync::Arc};
use virtdisk::{
    ImageFormat, RawDisk, RawWriter, ReadAt, compare_images, copy_image, detect_format,
};
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let end = start
            .checked_add(out.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        out.copy_from_slice(self.0.get(start..end).ok_or(io::ErrorKind::UnexpectedEof)?);
        Ok(())
    }
}
#[test]
fn detection_recognizes_signatures_and_keeps_raw_explicit() {
    assert_eq!(
        detect_format(&Bytes(b"QFI\xfb".to_vec())).unwrap(),
        Some(ImageFormat::Qcow2)
    );
    assert_eq!(
        detect_format(&Bytes(b"vhdxfile".to_vec())).unwrap(),
        Some(ImageFormat::Vhdx)
    );
    assert_eq!(
        detect_format(&Bytes(b"KDMV".to_vec())).unwrap(),
        Some(ImageFormat::Vmdk)
    );
    let mut vdi = vec![0; 68];
    vdi[64..68].copy_from_slice(&0xbeda107fu32.to_le_bytes());
    assert_eq!(detect_format(&Bytes(vdi)).unwrap(), Some(ImageFormat::Vdi));
    assert_eq!(detect_format(&Bytes(vec![0; 128])).unwrap(), None);
    assert_eq!(detect_format(&Bytes(vec![])).unwrap(), None);
}
#[test]
fn copying_preserves_content_and_comparison_checks_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("output.raw");
    let bytes = Arc::new(Bytes((0..100_000).map(|i| (i % 251) as u8).collect()));
    let output = RawWriter::create(&path, bytes.len()).unwrap();
    copy_image(bytes.as_ref(), &output).unwrap();
    output.flush().unwrap();
    let read = RawDisk::open(&path).unwrap();
    assert!(compare_images(bytes.as_ref(), &read).unwrap());
    assert!(!compare_images(bytes.as_ref(), &Bytes(vec![0; 100_000])).unwrap());
    assert!(!compare_images(bytes.as_ref(), &Bytes(vec![0; 10])).unwrap());
}

#[test]
fn opening_requires_explicit_raw_and_rejects_malformed_known_images() {
    use virtdisk::Image;
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    std::fs::write(&raw, [3; 512]).unwrap();
    assert!(Image::open(&raw, None).is_err());
    let disk = Image::open(&raw, Some(ImageFormat::Raw)).unwrap();
    assert_eq!(disk.info().format, ImageFormat::Raw);
    assert_eq!(disk.info().virtual_size, 512);
    assert_eq!(disk.info().container_size, 512);
    assert_eq!(disk.len(), 512);
    let invalid = dir.path().join("broken.qcow2");
    std::fs::write(&invalid, b"QFI\xfb").unwrap();
    assert!(Image::open(&invalid, None).is_err());
    assert!(Image::open(&raw, Some(ImageFormat::Vdi)).is_err());
}

#[test]
fn conversion_publishes_only_complete_new_output_and_preserves_existing() {
    use virtdisk::{Image, convert_image};
    let dir = tempfile::tempdir().unwrap();
    let source = Bytes(vec![7; 1024]);
    for format in [
        ImageFormat::Raw,
        ImageFormat::Qcow2,
        ImageFormat::Vdi,
        ImageFormat::Vmdk,
        ImageFormat::Vhdx,
    ] {
        let path = dir.path().join(format!("{format:?}"));
        convert_image(&source, &path, format).unwrap();
        let disk = Image::open(&path, Some(format)).unwrap();
        assert!(compare_images(&source, &disk).unwrap());
        assert!(convert_image(&Bytes(vec![0; 1024]), &path, format).is_err());
        assert!(compare_images(&source, &disk).unwrap());
    }
    struct Broken;
    impl ReadAt for Broken {
        fn len(&self) -> u64 {
            1024
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }
    let failed = dir.path().join("failed.raw");
    assert!(convert_image(&Broken, &failed, ImageFormat::Raw).is_err());
    assert!(!failed.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 5);
}

#[test]
fn generic_copy_propagates_writer_failure_and_validates_capacity_first() {
    use virtdisk::WriteAt;
    struct Reject(u64);
    impl WriteAt for Reject {
        fn len(&self) -> u64 {
            self.0
        }
        fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "injected full"))
        }
        fn flush(&self) -> io::Result<()> {
            Ok(())
        }
    }
    let source = Bytes(vec![1; 512]);
    assert_eq!(
        copy_image(&source, &Reject(512)).unwrap_err().kind(),
        io::ErrorKind::StorageFull
    );
    assert_eq!(
        copy_image(&source, &Reject(511)).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn resize_to_new_image_zeros_growth_and_requires_explicit_tail_policy() {
    use virtdisk::{Image, ShrinkPolicy, resize_image};
    let directory = tempfile::tempdir().unwrap();
    let source = Bytes(vec![9; 512]);
    let grown = directory.path().join("grown.qcow2");
    resize_image(
        &source,
        &grown,
        ImageFormat::Qcow2,
        1024,
        ShrinkPolicy::Reject,
    )
    .unwrap();
    let disk = Image::open(&grown, None).unwrap();
    assert!(compare_images(&disk, &Bytes([vec![9; 512], vec![0; 512]].concat())).unwrap());
    let rejected = directory.path().join("rejected.raw");
    assert!(
        resize_image(
            &source,
            &rejected,
            ImageFormat::Raw,
            256,
            ShrinkPolicy::Reject
        )
        .is_err()
    );
    assert!(
        resize_image(
            &source,
            &rejected,
            ImageFormat::Raw,
            256,
            ShrinkPolicy::RequireZero
        )
        .is_err()
    );
    assert!(!rejected.exists());
    resize_image(
        &source,
        &rejected,
        ImageFormat::Raw,
        256,
        ShrinkPolicy::AllowDataLoss,
    )
    .unwrap();
    assert_eq!(std::fs::read(rejected).unwrap(), vec![9; 256]);
    let zero_tail = Bytes([vec![9; 512], vec![0; 512]].concat());
    let safe = directory.path().join("zero-tail.vdi");
    resize_image(
        &zero_tail,
        &safe,
        ImageFormat::Vdi,
        512,
        ShrinkPolicy::RequireZero,
    )
    .unwrap();
    assert!(compare_images(&Image::open(safe, None).unwrap(), &source).unwrap());
}

#[test]
fn logical_hash_matches_standard_vector() {
    use virtdisk::hash_image;
    assert_eq!(
        hash_image(&Bytes(b"abc".to_vec())).unwrap(),
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
}

#[test]
fn extent_visitation_is_bounded_clipped_and_cancellable() {
    use virtdisk::{DiskExtent, DiskView, ExtentKind};
    struct Mapped;
    impl ReadAt for Mapped {
        fn len(&self) -> u64 {
            1024
        }
        fn read_exact_at(&self, _: u64, _: &mut [u8]) -> io::Result<()> {
            unreachable!()
        }
        fn visit_extents(
            &self,
            visit: &mut dyn FnMut(DiskExtent) -> io::Result<()>,
        ) -> io::Result<()> {
            visit(DiskExtent {
                offset: 0,
                length: 512,
                kind: ExtentKind::Allocated,
            })?;
            visit(DiskExtent {
                offset: 512,
                length: 512,
                kind: ExtentKind::Zero,
            })
        }
    }
    let view = DiskView::new(Arc::new(Mapped), 256, 512).unwrap();
    let mut extents = Vec::new();
    view.visit_extents(&mut |extent| {
        extents.push(extent);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        extents,
        vec![
            DiskExtent {
                offset: 0,
                length: 256,
                kind: ExtentKind::Allocated
            },
            DiskExtent {
                offset: 256,
                length: 256,
                kind: ExtentKind::Zero
            }
        ]
    );
    let mut count = 0;
    assert_eq!(
        view.visit_extents(&mut |_| {
            count += 1;
            Err(io::ErrorKind::Interrupted.into())
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(count, 1);
}

#[test]
fn copying_cancels_between_chunks_and_reports_partial_output() {
    use virtdisk::copy_image_with_cancel;
    let directory = tempfile::tempdir().unwrap();
    let output = RawWriter::create(directory.path().join("partial.raw"), 131072).unwrap();
    let mut checks = 0;
    let error = copy_image_with_cancel(&Bytes(vec![9; 131072]), &output, || {
        checks += 1;
        checks > 1
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    let mut bytes = vec![0; 131072];
    output.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..65536].iter().all(|b| *b == 9));
    assert!(bytes[65536..].iter().all(|b| *b == 0));
}

#[test]
fn conversion_verifies_logical_output_before_publication() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use virtdisk::convert_image;
    struct Changing(AtomicUsize);
    impl ReadAt for Changing {
        fn len(&self) -> u64 {
            512
        }
        fn read_exact_at(&self, _: u64, out: &mut [u8]) -> io::Result<()> {
            out.fill((self.0.fetch_add(1, Ordering::SeqCst) + 1) as u8);
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("changed.raw");
    assert!(convert_image(&Changing(AtomicUsize::new(0)), &output, ImageFormat::Raw).is_err());
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
