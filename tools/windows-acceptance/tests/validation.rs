use virtdisk_windows_acceptance::{FileIdentity, Manifest, Mode, validate};
fn manifest() -> Manifest {
    Manifest {
        version: 1,
        image: "child.vhdx".into(),
        parents: vec!["base.vhdx".into()],
        files: vec![
            FileIdentity {
                path: "base.vhdx".into(),
                length: 1,
                sha256: "00".repeat(32),
            },
            FileIdentity {
                path: "child.vhdx".into(),
                length: 1,
                sha256: "00".repeat(32),
            },
        ],
        capacity: 512,
        logical_sector: 512,
        physical_sector: 4096,
        logical_sha256: "00".repeat(32),
        mode: Mode::Clean,
    }
}
#[test]
fn rejects_escape_duplicate_missing_parent_and_invalid_geometry() {
    let dir = tempfile::tempdir().unwrap();
    for path in ["../outside", "/absolute", "C:\\host", "base:stream", "a\\b"] {
        let mut m = manifest();
        m.image = path.into();
        assert!(validate(dir.path(), &m).is_err());
    }
    let mut m = manifest();
    m.files.push(m.files[0].clone());
    assert!(validate(dir.path(), &m).is_err());
    let mut m = manifest();
    m.parents.push("missing".into());
    assert!(validate(dir.path(), &m).is_err());
    let mut m = manifest();
    m.logical_sector = 1024;
    assert!(validate(dir.path(), &m).is_err());
}
#[test]
fn rejects_identity_mismatch_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("base.vhdx"), [1]).unwrap();
    std::fs::write(dir.path().join("child.vhdx"), [1]).unwrap();
    assert!(validate(dir.path(), &manifest()).is_err());
    #[cfg(unix)]
    {
        std::fs::remove_file(dir.path().join("child.vhdx")).unwrap();
        std::os::unix::fs::symlink("base.vhdx", dir.path().join("child.vhdx")).unwrap();
        assert!(validate(dir.path(), &manifest()).is_err());
    }
}
#[test]
fn streaming_hash_is_standard_sha256() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data");
    std::fs::write(&path, b"abc").unwrap();
    assert_eq!(
        virtdisk_windows_acceptance::hash_file(&path).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn native_locator_profile_rejects_unlisted_alternate_paths() {
    use virtdisk_windows_acceptance::validate_locator_keys;
    assert!(
        validate_locator_keys(&[
            ("parent_linkage".into(), "id".into()),
            ("relative_path".into(), "base.vhdx".into())
        ])
        .is_ok()
    );
    assert!(
        validate_locator_keys(&[
            ("relative_path".into(), "base.vhdx".into()),
            ("absolute_win32_path".into(), "C:\\outside.vhdx".into())
        ])
        .is_err()
    );
}

#[test]
fn prepared_native_trees_validate_copy_and_preserve_originals() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("fixtures");
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_prepare"))
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    for sector in [512, 4096] {
        for image in ["base.vhdx", "child.vhdx"] {
            let path = root.join(format!("sector-{sector}/{image}.json"));
            let m: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let original = path.parent().unwrap();
            let copy = virtdisk_windows_acceptance::copy_tree(original, &m).unwrap();
            virtdisk_windows_acceptance::strict_locators(copy.path(), &m).unwrap();
            assert!(virtdisk_windows_acceptance::authorized_view(copy.path(), &m).is_ok());
            validate(original, &m).unwrap();
            #[cfg(not(windows))]
            {
                let output =
                    std::process::Command::new(env!("CARGO_BIN_EXE_virtdisk-windows-acceptance"))
                        .arg(&path)
                        .output()
                        .unwrap();
                assert!(!output.status.success());
                let artifact: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(artifact["gate"], "unfulfilled");
            }
        }
    }
}

#[test]
fn prepared_dirty_redo_matches_independent_models_and_preserves_sources() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("fixtures");
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_prepare"))
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    for sector in [512u32, 4096] {
        for mode in ["native_replay", "library_recovery"] {
            for image in ["base.vhdx", "child.vhdx"] {
                let directory = root.join(format!("sector-{sector}/{mode}/{image}"));
                let manifest: Manifest = serde_json::from_slice(
                    &std::fs::read(directory.join(format!("{image}.json"))).unwrap(),
                )
                .unwrap();
                let copy = virtdisk_windows_acceptance::copy_tree(&directory, &manifest).unwrap();
                assert!(
                    virtdisk::Vhdx::open_chain(
                        copy.path().join(image),
                        &manifest
                            .parents
                            .iter()
                            .map(|p| copy.path().join(p))
                            .collect::<Vec<_>>()
                    )
                    .is_err()
                );
                let mut expected = vec![7u8; 4 << 20];
                if image == "child.vhdx" {
                    expected[sector as usize - 3..sector as usize + 5].fill(9);
                    expected[(1 << 20) + 17..(1 << 20) + 20].fill(8);
                    expected[2 << 20..3 << 20].fill(0);
                    expected[(2 << 20) + 3] = 6;
                    expected[(3 << 20) + 1..(3 << 20) + 6].fill(0);
                }
                expected[8192..8200].fill(11);
                use sha2::{Digest, Sha256};
                assert_eq!(
                    manifest.logical_sha256,
                    format!("{:x}", Sha256::digest(&expected))
                );
                let recovered =
                    virtdisk_windows_acceptance::authorized_view(copy.path(), &manifest).unwrap();
                use virtdisk::ReadAt;
                let mut bytes = vec![0; expected.len()];
                recovered.read_exact_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, expected);
                drop(recovered);
                virtdisk::recover_vhdx_chain(
                    copy.path().join(image),
                    &manifest
                        .parents
                        .iter()
                        .map(|p| copy.path().join(p))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
                assert!(
                    virtdisk::Vhdx::open_chain(
                        copy.path().join(image),
                        &manifest
                            .parents
                            .iter()
                            .map(|p| copy.path().join(p))
                            .collect::<Vec<_>>()
                    )
                    .is_ok()
                );
                validate(&directory, &manifest).unwrap();
            }
        }
    }
}

#[test]
fn native_replay_requests_only_leaf_backing_write_and_readonly_surface() {
    use virtdisk_windows_acceptance::native_open_policy;
    let clean = native_open_policy(Mode::Clean);
    let recovered = native_open_policy(Mode::LibraryRecovery);
    let replay = native_open_policy(Mode::NativeReplay);
    assert_eq!(clean, recovered);
    assert_eq!(clean.rw_depth, 0);
    assert_eq!(clean.access_mask, 0x000d0000);
    assert_eq!(replay.rw_depth, 1);
    assert_eq!(replay.access_mask, clean.access_mask | 0x00020000);
    assert_eq!(replay.attach_flags, 1 | 2);
    assert_eq!(clean.attach_flags, replay.attach_flags);
    assert_eq!(replay.access_mask & (0x00100000 | 0x00200000), 0);
}

#[test]
fn native_producer_accepts_only_pinned_standalone_model_and_bounded_sector_writes() {
    use sha2::{Digest, Sha256};
    use virtdisk_windows_acceptance::producer_plan;
    for sector in [512u32, 4096] {
        let mut m = manifest();
        m.image = "base.vhdx".into();
        m.parents.clear();
        m.files.truncate(1);
        m.capacity = 4 << 20;
        m.logical_sector = sector;
        m.logical_sha256 = format!("{:x}", Sha256::digest(vec![7u8; 4 << 20]));
        let plan = producer_plan(&m).unwrap();
        assert_eq!(plan.len(), 3);
        for (offset, bytes) in plan {
            assert!(offset.is_multiple_of(u64::from(sector)));
            assert_eq!(bytes.len(), sector as usize);
            assert!(offset + bytes.len() as u64 <= m.capacity);
        }
        m.mode = Mode::NativeReplay;
        assert!(producer_plan(&m).is_err());
        m.mode = Mode::Clean;
        m.logical_sha256 = "00".repeat(32);
        assert!(producer_plan(&m).is_err());
    }
}

#[test]
fn rust_mutation_workload_preserves_parents_and_matches_every_model_byte() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = dir.path().join("fixtures");
    assert!(
        std::process::Command::new(env!("CARGO_BIN_EXE_prepare"))
            .arg(&fixtures)
            .status()
            .unwrap()
            .success()
    );
    for sector in [512, 4096] {
        for image in ["base.vhdx", "child.vhdx"] {
            let root = fixtures.join(format!("sector-{sector}"));
            let manifest: Manifest =
                serde_json::from_slice(&std::fs::read(root.join(format!("{image}.json"))).unwrap())
                    .unwrap();
            let copy = virtdisk_windows_acceptance::copy_tree(&root, &manifest).unwrap();
            let parent = copy.path().join("base.vhdx");
            let hash = virtdisk_windows_acceptance::hash_file(&parent).unwrap();
            let cases =
                virtdisk_windows_acceptance::mutation::exercise(copy.path(), &manifest).unwrap();
            assert_eq!(cases.len(), 3);
            for case in cases {
                use virtdisk::ReadAt;
                let view = virtdisk::Vhdx::open_chain(
                    copy.path().join(&case.image),
                    &case
                        .parents
                        .iter()
                        .map(|p| copy.path().join(p))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
                let mut bytes = vec![0; case.expected.len()];
                view.read_exact_at(0, &mut bytes).unwrap();
                assert_eq!(bytes, case.expected);
            }
            if image == "child.vhdx" {
                assert_eq!(
                    virtdisk_windows_acceptance::hash_file(&parent).unwrap(),
                    hash
                );
            }
            validate(&root, &manifest).unwrap();
        }
    }
}
