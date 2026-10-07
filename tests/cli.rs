#![cfg(feature = "cli")]
use std::process::Command;

#[cfg(target_os = "linux")]
#[test]
fn cli_vhdx_zero_uses_partial_sector_mapping_without_changing_parent() {
    use virtdisk::{ReadAt, VhdxWriter};
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vhdx");
    let child = directory.path().join("child.vhdx");
    let base = VhdxWriter::create(&parent, 1048576).unwrap();
    base.write_all_at(0, &vec![31; 1048576]).unwrap();
    base.flush().unwrap();
    drop(base);
    drop(VhdxWriter::create_overlay(&child, &parent, &[]).unwrap());
    let original = std::fs::read(&parent).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("zero")
        .arg(&child)
        .args(["vhdx", "511", "3"])
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let image = virtdisk::Image::open_chain(
        &child,
        Some(virtdisk::ImageFormat::Vhdx),
        std::slice::from_ref(&parent),
    )
    .unwrap();
    let mut actual = vec![0; 1048576];
    image.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![31; 1048576];
    expected[511..514].fill(0);
    assert_eq!(actual, expected);
    assert_eq!(std::fs::read(parent).unwrap(), original);
    let bytes = std::fs::read(child).unwrap();
    assert_eq!(
        u64::from_le_bytes(bytes[2097152..2097160].try_into().unwrap()) & 7,
        7
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cli_vmdk_native_trim_masks_parent_and_preserves_partial_write_neighbors() {
    use virtdisk::{ReadAt, VmdkWriter};
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vmdk");
    let child = directory.path().join("child.vmdk");
    let base = VmdkWriter::create_sparse(&parent, 131072).unwrap();
    base.write_all_at(0, &vec![31; 131072]).unwrap();
    base.flush().unwrap();
    drop(base);
    drop(VmdkWriter::create_overlay(&child, &parent, std::slice::from_ref(&parent)).unwrap());
    let parent_bytes = std::fs::read(&parent).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("trim")
        .arg(&child)
        .args(["vmdk", "0", "65536", "require"])
        .arg(&parent)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("deallocated")
    );
    let writer = VmdkWriter::open_chain(&child, std::slice::from_ref(&parent)).unwrap();
    writer.write_all_at(7, &[42; 3]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let reader = virtdisk::Image::open_chain(
        &child,
        Some(virtdisk::ImageFormat::Vmdk),
        std::slice::from_ref(&parent),
    )
    .unwrap();
    let mut bytes = vec![0; 131072];
    reader.read_exact_at(0, &mut bytes).unwrap();
    assert!(bytes[..7].iter().all(|b| *b == 0));
    assert_eq!(&bytes[7..10], &[42; 3]);
    assert!(bytes[10..65536].iter().all(|b| *b == 0));
    assert!(bytes[65536..].iter().all(|b| *b == 31));
    assert_eq!(std::fs::read(parent).unwrap(), parent_bytes);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_reverts_and_deletes_exact_binary_snapshot_id() {
    use std::sync::Arc;
    use virtdisk::{Qcow2, Qcow2Writer, RawDisk, ReadAt};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let mut writer = Qcow2Writer::create_sparse(&path, 65536).unwrap();
    writer.write_all_at(0, &[42; 512]).unwrap();
    writer.create_snapshot(&[255], b"saved").unwrap();
    writer.write_all_at(0, &[17; 512]).unwrap();
    writer.flush().unwrap();
    drop(writer);
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let invoke = |action: &str, id: &str| {
        Command::new(executable)
            .args(["snapshot", action])
            .arg(&path)
            .arg(id)
            .output()
            .unwrap()
    };
    let result = invoke("revert", "hex:ff");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("\"id_hex\":\"ff\"")
    );
    let image = Arc::new(Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap());
    let mut bytes = [0; 512];
    image.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [42; 512]);
    assert_eq!(image.list_snapshots().unwrap().len(), 1);
    drop(image);
    let original = std::fs::read(&path).unwrap();
    assert!(!invoke("delete", "missing").status.success());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(invoke("delete", "hex:ff").status.success());
    let image = Qcow2::open(Arc::new(RawDisk::open(&path).unwrap())).unwrap();
    image.validate_active_mapping().unwrap();
    assert!(image.list_snapshots().unwrap().is_empty());
    image.read_exact_at(0, &mut bytes).unwrap();
    assert_eq!(bytes, [42; 512]);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_creates_disk_snapshot_and_rejects_duplicate_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let writer = virtdisk::Qcow2Writer::create(&path, 65536).unwrap();
    writer.write_all_at(0, &[42; 512]).unwrap();
    drop(writer);
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let create = || {
        Command::new(executable)
            .args(["snapshot", "create"])
            .arg(&path)
            .args(["1", "saved"])
            .output()
            .unwrap()
    };
    let result = create();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("\"id_hex\":\"31\"")
    );
    let before = std::fs::read(&path).unwrap();
    assert!(!create().status.success());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for id in ["hex:z0", "hex:0", "hex:"] {
        assert!(
            !Command::new(executable)
                .args(["snapshot", "create"])
                .arg(&path)
                .args([id, "name"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    assert!(
        Command::new(executable)
            .args(["snapshot", "create"])
            .arg(&path)
            .args(["hex:ff", "hex:fe00"])
            .status()
            .unwrap()
            .success()
    );
    let reader =
        virtdisk::Qcow2::open(std::sync::Arc::new(virtdisk::RawDisk::open(&path).unwrap()))
            .unwrap();
    let snapshots = reader.list_snapshots().unwrap();
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.id == [255] && snapshot.name == [254, 0])
    );
}

#[test]
fn cli_check_reports_scope_and_rejects_corrupt_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    std::fs::write(&raw, [42; 512]).unwrap();
    let disk = directory.path().join("disk.qcow2");
    drop(virtdisk::Qcow2Writer::create(&disk, 65536).unwrap());
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let output = Command::new(executable)
        .arg("check")
        .arg(&raw)
        .args(["raw", "payload"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("\"structural_scope\":\"raw-length-only\""));
    assert!(text.contains("\"payload_bytes_read\":512"));
    let mut bytes = std::fs::read(&disk).unwrap();
    let table = u64::from_be_bytes(bytes[48..56].try_into().unwrap()) as usize;
    let block = u64::from_be_bytes(bytes[table..table + 8].try_into().unwrap()) as usize;
    bytes[block..block + 2].fill(0);
    std::fs::write(&disk, &bytes).unwrap();
    let failed = Command::new(executable)
        .arg("check")
        .arg(&disk)
        .args(["qcow2", "structure"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    assert_eq!(std::fs::read(&disk).unwrap(), bytes);
}

#[test]
fn cli_snapshot_listing_and_missing_export_are_read_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let output = directory.path().join("saved.raw");
    drop(virtdisk::Qcow2Writer::create(&path, 65536).unwrap());
    let original = std::fs::read(&path).unwrap();
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let list = Command::new(executable)
        .args(["snapshot", "list"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(list.stdout, b"[]\n");
    let export = Command::new(executable)
        .args(["snapshot", "export"])
        .arg(&path)
        .arg("missing")
        .arg(&output)
        .arg("raw")
        .output()
        .unwrap();
    assert!(!export.status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
#[ignore = "requires independent QEMU internal snapshot oracle"]
fn cli_exports_saved_snapshot_instead_of_current_disk() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.qcow2");
    let output = directory.path().join("saved.raw");
    assert!(
        Command::new("qemu-img")
            .args(["create", "-f", "qcow2"])
            .arg(&path)
            .arg("64K")
            .status()
            .unwrap()
            .success()
    );
    let write = |pattern| {
        assert!(
            Command::new("qemu-io")
                .args(["-f", "qcow2", "-c", &format!("write -P {pattern} 0 65536")])
                .arg(&path)
                .output()
                .unwrap()
                .status
                .success()
        )
    };
    write(42);
    assert!(
        Command::new("qemu-img")
            .args(["snapshot", "-c", "saved"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    write(17);
    let original = std::fs::read(&path).unwrap();
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let list = Command::new(executable)
        .args(["snapshot", "list"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(list.status.success());
    let text = String::from_utf8(list.stdout).unwrap();
    assert!(text.contains("\"id_hex\":\"31\""));
    assert!(text.contains("\"name_hex\":\"7361766564\""));
    let export = || {
        Command::new(executable)
            .args(["snapshot", "export"])
            .arg(&path)
            .arg("1")
            .arg(&output)
            .arg("raw")
            .output()
            .unwrap()
    };
    assert!(export().status.success());
    assert_eq!(std::fs::read(&output).unwrap(), vec![42; 65536]);
    assert!(!export().status.success());
    let hex_output = directory.path().join("saved-hex.raw");
    assert!(
        Command::new(executable)
            .args(["snapshot", "export"])
            .arg(&path)
            .arg("hex:31")
            .arg(&hex_output)
            .arg("raw")
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(std::fs::read(hex_output).unwrap(), vec![42; 65536]);
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
#[test]
fn cli_inspects_converts_and_compares_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.raw");
    let output = dir.path().join("out.qcow2");
    std::fs::write(&input, vec![13; 512]).unwrap();
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let info = Command::new(executable)
        .args(["info", input.to_str().unwrap(), "raw"])
        .output()
        .unwrap();
    assert!(info.status.success());
    let info = String::from_utf8(info.stdout).unwrap();
    assert!(info.contains("\"virtual_size\":512"));
    assert!(info.contains("\"write_supported\":false"));
    let convert = || {
        Command::new(executable)
            .args([
                "convert",
                input.to_str().unwrap(),
                "raw",
                output.to_str().unwrap(),
                "qcow2",
            ])
            .output()
            .unwrap()
    };
    assert!(convert().status.success());
    assert!(!convert().status.success());
    assert!(
        Command::new(executable)
            .args([
                "compare",
                input.to_str().unwrap(),
                "raw",
                output.to_str().unwrap(),
                "qcow2"
            ])
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(&input, vec![14; 512]).unwrap();
    assert!(
        !Command::new(executable)
            .args([
                "compare",
                input.to_str().unwrap(),
                "raw",
                output.to_str().unwrap(),
                "qcow2"
            ])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn cli_hashes_maps_and_resizes_with_explicit_shrink_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.raw");
    let output = directory.path().join("grown.vhdx");
    std::fs::write(&path, vec![3; 512]).unwrap();
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let hash = Command::new(executable)
        .args(["hash", path.to_str().unwrap(), "raw"])
        .output()
        .unwrap();
    assert!(hash.status.success());
    assert_eq!(hash.stdout.len(), 65);
    let map = Command::new(executable)
        .args(["map", path.to_str().unwrap(), "raw"])
        .output()
        .unwrap();
    assert!(map.status.success());
    assert!(
        String::from_utf8(map.stdout)
            .unwrap()
            .contains("\"length\":512")
    );
    assert!(
        Command::new(executable)
            .args([
                "resize",
                path.to_str().unwrap(),
                "raw",
                output.to_str().unwrap(),
                "vhdx",
                "1024",
                "reject"
            ])
            .status()
            .unwrap()
            .success()
    );
    let rejected = directory.path().join("rejected.raw");
    assert!(
        !Command::new(executable)
            .args([
                "resize",
                path.to_str().unwrap(),
                "raw",
                rejected.to_str().unwrap(),
                "raw",
                "256",
                "zero-tail"
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(!rejected.exists());
}

#[test]
fn cli_compacts_to_verified_new_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.raw");
    let output = directory.path().join("compact.qcow2");
    std::fs::write(&input, vec![0; 131072]).unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg("compact")
            .arg(&input)
            .arg("raw")
            .arg(&output)
            .arg("qcow2")
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    assert!(!run().status.success());
    assert!(
        virtdisk::compare_images(
            &virtdisk::RawDisk::open(input).unwrap(),
            &virtdisk::Image::open(output, Some(virtdisk::ImageFormat::Qcow2)).unwrap()
        )
        .unwrap()
    );
}

#[test]
fn cli_trim_has_explicit_policy_and_zero_accepts_authorized_parent_paths() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&base, vec![7; 65536]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 65536).unwrap();
    let executable = env!("CARGO_BIN_EXE_virtdisk");
    let result = Command::new(executable)
        .arg("trim")
        .arg(&child)
        .args(["qcow2", "0", "65536", "require"])
        .arg(&base)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("deallocated")
    );
    let raw = directory.path().join("raw.img");
    std::fs::write(&raw, vec![7; 512]).unwrap();
    assert!(
        Command::new(executable)
            .arg("zero")
            .arg(&raw)
            .args(["raw", "10", "20"])
            .status()
            .unwrap()
            .success()
    );
    let bytes = std::fs::read(raw).unwrap();
    assert_eq!(&bytes[..10], &[7; 10]);
    assert_eq!(&bytes[10..30], &[0; 20]);
    assert_eq!(&bytes[30..], &[7; 482]);
    assert_eq!(std::fs::read(base).unwrap(), vec![7; 65536]);
}

#[test]
fn cli_rejects_invalid_mutation_requests_before_touching_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("input.raw");
    let bytes = vec![9; 512];
    std::fs::write(&path, &bytes).unwrap();
    for arguments in [
        vec!["trim", "raw", "0", "512", "implicit"],
        vec!["zero", "raw", "500", "13"],
        vec!["zero", "raw", "-1", "20"],
        vec!["trim", "raw", "0", "18446744073709551615", "require"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg(arguments[0])
            .arg(&path)
            .args(&arguments[1..])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn cli_zero_masks_an_authorized_parent_and_preserves_neighbors() {
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.raw");
    let child = directory.path().join("child.qcow2");
    std::fs::write(&base, vec![7; 65536]).unwrap();
    virtdisk::create_qcow2_overlay(&child, &base, "raw", 65536).unwrap();
    let execute = |authorize: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command.arg("zero").arg(&child).args(["qcow2", "10", "20"]);
        if authorize {
            command.arg(&base);
        }
        command.output().unwrap()
    };
    let original = std::fs::read(&child).unwrap();
    assert_eq!(execute(false).status.code(), Some(2));
    assert_eq!(std::fs::read(&child).unwrap(), original);
    assert!(execute(true).status.success());
    let image = virtdisk::Qcow2::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let mut actual = vec![0; 65536];
    image.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![7; 65536];
    expected[10..30].fill(0);
    assert_eq!(actual, expected);
    assert_eq!(std::fs::read(base).unwrap(), vec![7; 65536]);
}

#[test]
fn cli_chain_inspection_hash_and_map_require_explicit_parent_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().join("parent.vdi");
    let child = directory.path().join("child.vdi");
    drop(virtdisk::VdiWriter::create(&parent, 1048576).unwrap());
    virtdisk::create_vdi_overlay(&child, &parent, &[]).unwrap();
    for operation in ["info", "hash", "map"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        command.arg(operation).arg(&child).arg("vdi");
        assert_eq!(command.output().unwrap().status.code(), Some(2));
        let output = command.arg(&parent).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if operation == "info" {
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("\"has_parent\":true")
            );
        } else if operation == "hash" {
            assert_eq!(output.stdout.len(), 65);
        } else {
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("inherited")
            );
        }
    }
}

#[test]
fn cli_creates_explicit_profiles_and_never_overwrites_existing_outputs() {
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    for (name, format) in [
        ("raw", virtdisk::ImageFormat::Raw),
        ("qcow2", virtdisk::ImageFormat::Qcow2),
        ("vdi", virtdisk::ImageFormat::Vdi),
        ("vmdk", virtdisk::ImageFormat::Vmdk),
        ("vhdx", virtdisk::ImageFormat::Vhdx),
    ] {
        let path = directory.path().join(name);
        let execute = || {
            Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .arg("create")
                .arg(&path)
                .args([name, "65536"])
                .output()
                .unwrap()
        };
        let result = execute();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let original = std::fs::read(&path).unwrap();
        assert_eq!(execute().status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let image = virtdisk::Image::open(&path, Some(format)).unwrap();
        let mut bytes = vec![1; 65536];
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, vec![0; 65536]);
    }
    let invalid = directory.path().join("invalid");
    assert_eq!(
        Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg("create")
            .arg(&invalid)
            .args(["vdi", "513"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    assert!(!invalid.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn cli_preallocation_preserves_payload_and_requests_host_storage() {
    use std::os::unix::fs::MetadataExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    let writer = virtdisk::RawWriter::create(&path, 1048576).unwrap();
    writer.write_all_at(7, &[19; 12]).unwrap();
    drop(writer);
    let bytes = std::fs::read(&path).unwrap();
    let before = std::fs::metadata(&path).unwrap().blocks();
    let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("preallocate")
        .arg(&path)
        .args(["raw", "0", "1048576"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(std::fs::metadata(&path).unwrap().blocks() > before);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_native_resize_requires_tail_policy_and_zeroes_grown_range() {
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    for (selected, format) in [
        ("raw", virtdisk::ImageFormat::Raw),
        ("qcow2", virtdisk::ImageFormat::Qcow2),
        ("vdi", virtdisk::ImageFormat::Vdi),
        ("vmdk", virtdisk::ImageFormat::Vmdk),
        ("vhdx", virtdisk::ImageFormat::Vhdx),
    ] {
        let path = directory.path().join(format!("image.{selected}"));
        let writer = virtdisk::ImageWriter::create_sparse(&path, format, 131072).unwrap();
        virtdisk::WriteAt::write_all_at(&writer, 65536, &[29; 512]).unwrap();
        drop(writer);
        let execute = |size: &str, policy: &str| {
            Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .arg("resize-native")
                .arg(&path)
                .args([selected, size, policy])
                .output()
                .unwrap()
        };
        let original = std::fs::read(&path).unwrap();
        assert_eq!(execute("65536", "reject").status.code(), Some(2));
        assert_eq!(execute("65536", "zero-tail").status.code(), Some(2));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(execute("65536", "allow-loss").status.success());
        let result = execute("196608", "reject");
        assert!(
            result.status.success(),
            "{selected}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let image = virtdisk::Image::open(&path, Some(format)).unwrap();
        assert_eq!(image.len(), 196608);
        let mut bytes = vec![1; 196608];
        image.read_exact_at(0, &mut bytes).unwrap();
        assert_eq!(bytes, vec![0; 196608]);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cli_vdi_trim_reclaims_child_tail_masks_parent_and_preserves_other_block() {
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("base.vdi");
    let child = directory.path().join("child.vdi");
    let writer = virtdisk::VdiWriter::create(&base, 2097152).unwrap();
    writer.write_all_at(0, &[41; 512]).unwrap();
    drop(writer);
    let parent_bytes = std::fs::read(&base).unwrap();
    let writer = virtdisk::VdiWriter::create_overlay(&child, &base, &[]).unwrap();
    writer.write_all_at(0, &[51; 512]).unwrap();
    writer.write_all_at(1048576, &[61; 512]).unwrap();
    drop(writer);
    let before = std::fs::metadata(&child).unwrap().len();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("trim")
        .arg(&child)
        .args(["vdi", "0", "1048576", "require"])
        .arg(&base)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("deallocated")
    );
    assert_eq!(std::fs::metadata(&child).unwrap().len(), before - 1048576);
    assert_eq!(std::fs::read(&base).unwrap(), parent_bytes);
    let image =
        virtdisk::Image::open_chain(&child, Some(virtdisk::ImageFormat::Vdi), &[base]).unwrap();
    let mut actual = vec![0; 2097152];
    image.read_exact_at(0, &mut actual).unwrap();
    let mut expected = vec![0; 2097152];
    expected[1048576..1049088].fill(61);
    assert_eq!(actual, expected);
}

#[cfg(target_os = "linux")]
#[test]
fn cli_sparse_vdi_creation_enables_native_capacity_changes() {
    use virtdisk::ReadAt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("disk.vdi");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg("create-sparse")
            .arg(&path)
            .args(["vdi", "1048576"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(std::fs::metadata(&path).unwrap().len() < 1048576);
    assert!(
        Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .arg("resize-native")
            .arg(&path)
            .args(["vdi", "2097152", "reject"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let image = virtdisk::Image::open(path, Some(virtdisk::ImageFormat::Vdi)).unwrap();
    assert_eq!(image.len(), 2097152);
}
