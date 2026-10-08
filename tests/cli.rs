#![cfg(feature = "std")]
#![cfg(feature = "cli")]
use std::process::Command;

// A concurrent subprocess spawn can inherit this process's locked descriptors
// until exec closes them. Keep each test's image handles and subprocesses inside
// one process boundary so drop/reopen checks cannot see another test's fork.
static PROCESS_BOUNDARY: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn subprocess_test() -> std::sync::MutexGuard<'static, ()> {
    PROCESS_BOUNDARY
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

#[test]
fn json_errors_report_chain_and_descriptor_bounds_as_parser_limits() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    let parent = directory.path().join("parent.qcow2");
    let child = directory.path().join("child.qcow2");
    let vmdk = directory.path().join("image.vmdk");
    std::fs::write(&raw, [37; 65536]).unwrap();
    let source = virtdisk::RawDisk::open(&raw).unwrap();
    virtdisk::convert_image(&source, &parent, virtdisk::ImageFormat::Qcow2).unwrap();
    virtdisk::create_qcow2_overlay(&child, &parent, "qcow2", 65536).unwrap();
    virtdisk::convert_image(&source, &vmdk, virtdisk::ImageFormat::Vmdk).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "--parser-limit", "recursion=1", "info"])
        .arg(&child)
        .arg("qcow2")
        .arg(&parent)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let record = String::from_utf8(output.stderr).unwrap();
    assert!(record.contains("\"code\":\"parser-limit\",\"kind\":\"invalid-data\""));
    assert!(
        record.contains("\"resource\":\"recursion-depth\",\"limit\":\"1\",\"requested\":\"2\"")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "--parser-limit", "attribute=1", "info"])
        .arg(&vmdk)
        .arg("vmdk")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("\"resource\":\"attribute-bytes\",\"limit\":\"1\"")
    );
}

#[test]
fn json_errors_identify_parser_quota_through_read_provenance() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    std::fs::write(&raw, [37; 65537]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "--parser-limit", "work=1", "hash"])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let record = String::from_utf8(output.stderr).unwrap();
    assert!(record.contains("\"code\":\"parser-limit\""));
    assert!(record.contains("\"resource\":\"work-items\",\"limit\":\"1\",\"requested\":\"2\""));
    assert!(record.contains("\"kind\":\"unsupported\""));
    assert!(output.stdout.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn json_errors_report_recovery_and_coexist_with_progress() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    let qcow2 = directory.path().join("disk.qcow2");
    std::fs::write(&raw, [37; 512]).unwrap();
    virtdisk::convert_image(
        &virtdisk::RawDisk::open(&raw).unwrap(),
        &qcow2,
        virtdisk::ImageFormat::Qcow2,
    )
    .unwrap();
    let original = std::fs::read(&qcow2).unwrap();
    let mut journal = qcow2.as_os_str().to_os_string();
    journal.push(".virtdisk-qcow2-journal");
    let journal = std::path::PathBuf::from(journal);
    std::fs::write(&journal, b"pending evidence").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "zero"])
        .arg(&qcow2)
        .args(["qcow2", "0", "512"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("\"code\":\"recovery-required\""));
    assert_eq!(std::fs::read(&qcow2).unwrap(), original);
    assert_eq!(std::fs::read(&journal).unwrap(), b"pending evidence");
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--json-errors",
            "--progress",
            "--operation-limit",
            "bytes=511",
            "check",
        ])
        .arg(&raw)
        .args(["raw", "payload"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let records = String::from_utf8(output.stderr).unwrap();
    assert!(
        records
            .lines()
            .next()
            .unwrap()
            .starts_with("{\"type\":\"progress\"")
    );
    assert!(
        records
            .lines()
            .last()
            .unwrap()
            .contains("\"code\":\"resource-limit\"")
    );
    assert!(
        records
            .lines()
            .all(|line| line.starts_with('{') && line.ends_with('}'))
    );
    let plain = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("hash")
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    let json = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "hash"])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert!(json.status.success());
    assert_eq!(plain.stdout, json.stdout);
    assert!(json.stderr.is_empty());
}

#[test]
fn json_errors_preserve_mutation_ranges_and_escape_messages() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    std::fs::write(&raw, [37; 512]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "zero"])
        .arg(&raw)
        .args(["raw", "512", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let record = String::from_utf8(output.stderr).unwrap();
    assert!(
        record.contains(
            "\"operation\":\"write-zeroes\",\"format\":\"raw\",\"offset\":512,\"length\":1"
        )
    );
    assert_eq!(std::fs::read(&raw).unwrap(), [37; 512]);
    #[cfg(unix)]
    {
        let corrupt = directory.path().join("bad\"\\\n猫.qcow2");
        std::fs::write(&corrupt, b"QFI\xfb").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--json-errors", "info"])
            .arg(&corrupt)
            .arg("qcow2")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let record = String::from_utf8(output.stderr).unwrap();
        assert_eq!(record.lines().count(), 1);
        assert!(record.contains("bad\\\"\\\\\\n猫.qcow2"), "{record}");
    }
}

#[test]
fn json_errors_report_quota_and_input_kinds_without_success_output() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    std::fs::write(&raw, [37; 512]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--json-errors", "--operation-limit", "bytes=511", "hash"])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let record = String::from_utf8(output.stderr).unwrap();
    assert_eq!(record.lines().count(), 1);
    assert!(record.starts_with("{\"type\":\"error\",\"code\":\"resource-limit\""));
    assert!(record.contains("\"resource\":\"logical-bytes\""));
    assert!(record.contains("\"limit\":\"511\",\"requested\":\"512\""));
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--json-errors",
            "--operation-limit",
            "scratch=0",
            "hash",
            "absent",
            "raw",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("\"kind\":\"invalid-input\""));
}

#[test]
fn progress_handles_comparison_empty_work_and_rejects_unsupported_commands() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 512]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "compare"])
        .arg(&path)
        .arg("raw")
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let events = String::from_utf8(output.stderr).unwrap();
    assert!(
        events
            .lines()
            .last()
            .unwrap()
            .contains("\"io_operations\":2")
    );
    std::fs::write(&path, []).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "hash"])
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("\"completed_bytes\":0,\"total_bytes\":0")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "info", "absent", "raw"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("controls require"));
}

#[cfg(unix)]
#[test]
fn closed_progress_pipe_fails_without_a_success_result_or_panic() {
    let _process_boundary = subprocess_test();
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
    };
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    let original = vec![37; 1048576];
    std::fs::write(&raw, &original).unwrap();
    // Close the receiver before spawning, avoiding a race with progress writes.
    let (sender, receiver) = UnixStream::pair().unwrap();
    drop(receiver);
    let sender: OwnedFd = sender.into();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "--operation-limit", "scratch=1", "hash"])
        .arg(&raw)
        .arg("raw")
        .stdout(Stdio::piped())
        .stderr(Stdio::from(sender))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&raw).unwrap(), original);
}

#[test]
fn progress_reports_final_partial_chunks_on_stderr_without_changing_results() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 513]).unwrap();
    let plain = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("hash")
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    let progress = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--operation-limit", "scratch=256", "--progress", "hash"])
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert!(
        progress.status.success(),
        "{}",
        String::from_utf8_lossy(&progress.stderr)
    );
    assert_eq!(progress.stdout, plain.stdout);
    let events = String::from_utf8(progress.stderr).unwrap();
    assert_eq!(events.lines().count(), 4);
    assert!(
        events
            .lines()
            .all(|line| line.starts_with("{\"type\":\"progress\",\"phase\":\"processing\""))
    );
    let final_event = events.lines().last().unwrap();
    assert!(final_event.contains("\"completed_bytes\":513,\"total_bytes\":513"));
    assert!(final_event.contains("\"io_operations\":3"));
}

#[test]
fn check_progress_separates_metadata_and_payload_and_quota_refusal_is_not_success() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 512]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "check"])
        .arg(&path)
        .args(["raw", "payload"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let events = String::from_utf8(output.stderr).unwrap();
    assert!(
        events
            .contains("\"phase\":\"metadata-validation\",\"completed_bytes\":0,\"total_bytes\":0")
    );
    assert!(
        events.contains(
            "\"phase\":\"payload-validation\",\"completed_bytes\":512,\"total_bytes\":512"
        )
    );
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--progress", "--operation-limit", "bytes=511", "hash"])
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("\"type\":\"progress\""));
}

#[test]
fn operation_quotas_bound_hash_compare_and_payload_check() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    std::fs::write(&raw, [37; 1024]).unwrap();
    for command in ["hash", "compare", "check"] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        process
            .args(["--operation-limit", "bytes=1023", command])
            .arg(&raw)
            .arg("raw");
        if command == "compare" {
            process.arg(&raw).arg("raw");
        }
        if command == "check" {
            process.arg("payload");
        }
        let output = process.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("LogicalBytes requires 1024, limit 1023")
        );
        assert!(output.stdout.is_empty());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--operation-limit",
            "bytes=1024",
            "--operation-limit",
            "scratch=256",
            "--operation-limit",
            "io=4",
            "hash",
        ])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), 65);
}

#[test]
fn operation_controls_validate_before_file_access_and_allow_empty_work() {
    let _process_boundary = subprocess_test();
    for arguments in [
        vec!["--operation-limit", "scratch=0", "hash", "absent", "raw"],
        vec![
            "--operation-limit",
            "scratch=131073",
            "hash",
            "absent",
            "raw",
        ],
        vec!["--operation-limit", "unknown=1", "hash", "absent", "raw"],
        vec!["--operation-limit", "io=-1", "hash", "absent", "raw"],
        vec![
            "--operation-limit",
            "bytes=0",
            "resize-native",
            "absent",
            "raw",
            "output",
            "raw",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("No such file"));
    }
    let directory = tempfile::tempdir().unwrap();
    let empty = directory.path().join("empty");
    std::fs::write(&empty, []).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--operation-limit",
            "bytes=0",
            "--parser-limit",
            "work=1",
            "--operation-limit",
            "io=0",
            "hash",
        ])
        .arg(&empty)
        .arg("raw")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 65);
}

#[test]
fn comparison_io_and_combined_scratch_are_budgeted_and_repetition_tightens() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 512]).unwrap();
    for limit in ["io=1", "scratch=1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--operation-limit", limit, "compare"])
            .arg(&path)
            .arg("raw")
            .arg(&path)
            .arg("raw")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--operation-limit",
            "bytes=511",
            "--operation-limit",
            "bytes=512",
            "hash",
        ])
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("limit 511"));
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--operation-limit",
            "bytes=0",
            "--operation-limit",
            "io=0",
            "check",
        ])
        .arg(&path)
        .args(["raw", "structure"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"payload_bytes_read\":0"));
}

#[test]
fn snapshot_listing_and_export_honor_parser_limits() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let image = directory.path().join("disk.qcow2");
    // A valid saved-state fixture does not require host snapshot mutation support.
    let mut original = vec![0; 4096];
    original[..4].copy_from_slice(b"QFI\xfb");
    for (at, value) in [
        (4, 3u32),
        (20, 9),
        (36, 1),
        (56, 1),
        (60, 1),
        (96, 4),
        (100, 104),
        (3592, 1),
        (3620, 16),
    ] {
        original[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (at, value) in [
        (24, 512u64),
        (40, 512),
        (48, 1024),
        (64, 3584),
        (512, 1536),
        (1024, 2560),
        (1536, 2048),
        (3072, 1536 | (1 << 63)),
        (3584, 3072),
        (3632, 512),
    ] {
        original[at..at + 8].copy_from_slice(&value.to_be_bytes());
    }
    original[3596..3598].copy_from_slice(&2u16.to_be_bytes());
    original[3598..3600].copy_from_slice(&4u16.to_be_bytes());
    original[3640..3646].copy_from_slice(b"idname");
    original[2048..2560].fill(37);
    for (index, count) in [1u16, 1, 1, 2, 2, 1, 1, 1].into_iter().enumerate() {
        original[2560 + index * 2..2562 + index * 2].copy_from_slice(&count.to_be_bytes());
    }
    std::fs::write(&image, &original).unwrap();
    virtdisk::Qcow2::open(std::sync::Arc::new(
        virtdisk::RawDisk::open(&image).unwrap(),
    ))
    .unwrap()
    .validate_active_mapping()
    .unwrap();
    let listing = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "metadata=1", "snapshot", "list"])
        .arg(&image)
        .output()
        .unwrap();
    assert!(!listing.status.success());
    assert!(
        String::from_utf8_lossy(&listing.stderr)
            .contains("metadata exceeds configured parser limit")
    );
    let listing = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "work=100000", "snapshot", "list"])
        .arg(&image)
        .output()
        .unwrap();
    assert!(listing.status.success());
    assert!(String::from_utf8_lossy(&listing.stdout).contains("\"id_hex\":\"6964\""));
    let output = directory.path().join("export.raw");
    let refused = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "metadata=1", "snapshot", "export"])
        .arg(&image)
        .arg("id")
        .arg(&output)
        .arg("raw")
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("metadata exceeds configured parser limit")
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(&image).unwrap(), original);
}

#[test]
fn check_enforces_caller_metadata_and_payload_work_limits() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("raw");
    let qcow2 = directory.path().join("disk.qcow2");
    std::fs::write(&raw, [37; 131072]).unwrap();
    virtdisk::convert_image(
        &virtdisk::RawDisk::open(&raw).unwrap(),
        &qcow2,
        virtdisk::ImageFormat::Qcow2,
    )
    .unwrap();
    for (limit, path, format, mode, diagnostic) in [
        (
            "metadata=1",
            &qcow2,
            "qcow2",
            "structure",
            "metadata exceeds configured parser limit",
        ),
        (
            "work=1",
            &raw,
            "raw",
            "payload",
            "work exceeds configured parser limit",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--parser-limit", limit, "check"])
            .arg(path)
            .args([format, mode])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
        assert!(output.stdout.is_empty());
    }
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "work=1000", "check"])
        .arg(&raw)
        .args(["raw", "payload"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"payload_bytes_read\":131072"));
}

#[test]
fn reader_parser_limits_reach_opening_and_deferred_reads() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    let qcow2 = directory.path().join("source.qcow2");
    std::fs::write(&raw, [37; 131072]).unwrap();
    virtdisk::convert_image(
        &virtdisk::RawDisk::open(&raw).unwrap(),
        &qcow2,
        virtdisk::ImageFormat::Qcow2,
    )
    .unwrap();
    let original = std::fs::read(&qcow2).unwrap();
    let run = |limit: &str, command: &str, path: &std::path::Path, format: &str| {
        Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--parser-limit", limit, command])
            .arg(path)
            .arg(format)
            .output()
            .unwrap()
    };
    let refused = run("metadata=1", "info", &qcow2, "qcow2");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("metadata exceeds configured parser limit")
    );
    let refused = run("work=1", "hash", &raw, "raw");
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("work exceeds configured parser limit")
    );
    assert!(run("work=1000", "hash", &raw, "raw").status.success());
    let replay = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--replay-vhdx-log", "info"])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert!(!replay.status.success());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("immutable log replay requires VHDX"));
    assert_eq!(std::fs::read(&qcow2).unwrap(), original);
}

#[test]
fn invalid_reader_controls_are_refused_before_opening() {
    let _process_boundary = subprocess_test();
    for arguments in [
        vec!["--parser-limit", "work=0", "info", "absent", "raw"],
        vec!["--parser-limit", "unknown=1", "info", "absent", "raw"],
        vec![
            "--parser-limit",
            "metadata=18446744073709551615",
            "info",
            "absent",
            "raw",
        ],
        vec![
            "--parser-limit",
            "work=1",
            "zero",
            "absent",
            "raw",
            "0",
            "512",
        ],
        vec!["--replay-vhdx-log", "zero", "absent", "vhdx", "0", "512"],
        vec!["--replay-vhdx-log", "check", "absent", "vhdx", "structure"],
        vec!["--replay-vhdx-log", "snapshot", "list", "absent"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("No such file"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("parser"));
    }
}

#[test]
fn all_parser_fields_are_accepted_and_repetition_only_tightens() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let raw = directory.path().join("source.raw");
    std::fs::write(&raw, [37; 131072]).unwrap();
    for name in [
        "metadata",
        "cache",
        "recursion",
        "work",
        "decompressed",
        "decompression-buffer",
        "attribute",
        "attribute-list-records",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--parser-limit", &format!("{name}=1"), "info"])
            .arg(&raw)
            .arg("raw")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args([
            "--parser-limit",
            "work=1",
            "--parser-limit",
            "work=1000",
            "hash",
        ])
        .arg(&raw)
        .arg("raw")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("work exceeds configured parser limit")
    );
}

#[test]
fn source_parser_refusal_prevents_conversion_compaction_and_resize_outputs() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.qcow2");
    drop(virtdisk::ImageWriter::create(&source, virtdisk::ImageFormat::Qcow2, 65536).unwrap());
    let original = std::fs::read(&source).unwrap();
    for command in ["convert", "compact", "resize"] {
        let output = directory.path().join(command);
        let mut process = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        process
            .args(["--parser-limit", "metadata=1", command])
            .arg(&source)
            .arg("qcow2")
            .arg(&output)
            .arg("raw");
        if command == "resize" {
            process.args(["131072", "reject"]);
        }
        let result = process.output().unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("metadata exceeds configured parser limit")
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
fn deferred_reader_budget_failure_removes_unpublished_outputs() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.raw");
    let original = [37; 131072];
    std::fs::write(&source, original).unwrap();
    for command in ["convert", "compact", "resize"] {
        let output = directory.path().join(command);
        let mut process = Command::new(env!("CARGO_BIN_EXE_virtdisk"));
        process
            .args(["--parser-limit", "work=1", command])
            .arg(&source)
            .arg("raw")
            .arg(&output)
            .arg("raw");
        if command == "resize" {
            process.args(["131072", "reject"]);
        }
        let result = process.output().unwrap();
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("work exceeds configured parser limit")
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn mutation_commands_preserve_pending_evidence_until_recovery_is_authorized() {
    let _process_boundary = subprocess_test();
    use std::{ffi::OsString, fs};
    use virtdisk::{ImageFormat, ImageWriter, WriteAt};
    let directory = tempfile::tempdir().unwrap();
    for (format, name, suffix) in [
        (ImageFormat::Qcow2, "qcow2", ".virtdisk-qcow2-journal"),
        (ImageFormat::Vdi, "vdi", ".virtdisk-transaction"),
        (ImageFormat::Vmdk, "vmdk", ".virtdisk-transaction"),
    ] {
        let path = directory.path().join(name);
        let writer = ImageWriter::create_sparse(&path, format, 65536).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let original = fs::read(&path).unwrap();
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        fs::write(&sidecar, b"pending evidence").unwrap();
        let mut cases: Vec<Vec<OsString>> = vec![
            vec![
                "zero".into(),
                path.clone().into(),
                name.into(),
                "0".into(),
                "512".into(),
            ],
            vec![
                "trim".into(),
                path.clone().into(),
                name.into(),
                "0".into(),
                "512".into(),
                "zero-fallback".into(),
            ],
            vec![
                "preallocate".into(),
                path.clone().into(),
                name.into(),
                "0".into(),
                "512".into(),
            ],
            vec![
                "resize-native".into(),
                path.clone().into(),
                name.into(),
                "131072".into(),
                "reject".into(),
            ],
        ];
        if format == ImageFormat::Qcow2 {
            cases.push(vec![
                "snapshot".into(),
                "create".into(),
                path.clone().into(),
                "id".into(),
                "name".into(),
            ]);
            for action in ["delete", "revert"] {
                cases.push(vec![
                    "snapshot".into(),
                    action.into(),
                    path.clone().into(),
                    "id".into(),
                ]);
            }
        }
        for arguments in cases {
            let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(arguments)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("image requires explicitly authorized recovery")
            );
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::read(&sidecar).unwrap(), b"pending evidence");
        }
    }
}

#[test]
fn recovery_flag_is_rejected_for_readers_and_new_outputs() {
    let _process_boundary = subprocess_test();
    for arguments in [
        vec!["--recover", "info", "absent", "raw"],
        vec!["--recover", "create", "absent", "raw", "512"],
        vec!["--recover", "snapshot", "list", "absent"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--recover requires an existing-image mutation command")
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn cli_vhdx_zero_uses_partial_sector_mapping_without_changing_parent() {
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    assert!(info.contains("\"container_size\":512"));
    assert!(info.contains("\"container_set_size\":512"));
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    #[cfg(target_os = "linux")]
    {
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
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("requires Linux"));
        let reader = virtdisk::Qcow2::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        let mut inherited = [0; 512];
        virtdisk::ReadAt::read_exact_at(&reader, 0, &mut inherited).unwrap();
        assert_eq!(inherited, [7; 512]);
    }
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
    #[cfg(target_os = "linux")]
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
    let authorized = execute(true);
    #[cfg(target_os = "linux")]
    {
        assert!(
            authorized.status.success(),
            "{}",
            String::from_utf8_lossy(&authorized.stderr)
        );
        let image = virtdisk::Qcow2::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        let mut actual = vec![0; 65536];
        image.read_exact_at(0, &mut actual).unwrap();
        let mut expected = vec![7; 65536];
        expected[10..30].fill(0);
        assert_eq!(actual, expected);
    }
    #[cfg(not(target_os = "linux"))]
    {
        assert_eq!(authorized.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&authorized.stderr).contains("requires Linux"));
        assert_eq!(std::fs::read(&child).unwrap(), original);
    }
    assert_eq!(std::fs::read(base).unwrap(), vec![7; 65536]);
}

#[test]
fn cli_chain_inspection_hash_and_map_require_explicit_parent_authorization() {
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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
    let _process_boundary = subprocess_test();
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

#[test]
fn cli_descriptor_sizes_preserve_primary_and_add_full_extent_aggregate() {
    let _process_boundary = subprocess_test();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.vmdk");
    let extent = dir.path().join("flat.vmdk");
    std::fs::write(&extent, vec![31; 2048]).unwrap();
    std::fs::write(&path, "version=1\nCID=12345678\nparentCID=ffffffff\ncreateType=\"monolithicFlat\"\nRW 2 FLAT \"flat.vmdk\" 1\n").unwrap();
    let primary = std::fs::metadata(&path).unwrap().len();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .arg("info")
        .arg(&path)
        .arg("vmdk")
        .arg(&extent)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains(&format!("\"container_size\":{primary},")));
    assert!(json.contains(&format!("\"container_set_size\":{},", primary + 2048)));
    assert!(json.contains("\"virtual_size\":1024,"));
}

#[test]
fn conversion_and_compaction_progress_and_quotas_preserve_publication_contract() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("source");
    std::fs::write(&input, [37; 512]).unwrap();
    for command in ["convert", "compact"] {
        for format in ["raw", "qcow2", "vhdx", "vdi", "vmdk"] {
            let output = directory.path().join(format!("{command}-{format}"));
            let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(["--progress", command])
                .arg(&input)
                .arg("raw")
                .arg(&output)
                .arg(format)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(result.stdout.is_empty());
            let progress = String::from_utf8(result.stderr).unwrap();
            assert!(progress.contains("\"phase\":\"output-verification\""));
            assert!(
                progress
                    .lines()
                    .last()
                    .unwrap()
                    .contains("\"phase\":\"publication\"")
            );
            let refused = directory.path().join(format!("refused-{command}-{format}"));
            let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(["--json-errors", "--operation-limit", "bytes=512", command])
                .arg(&input)
                .arg("raw")
                .arg(&refused)
                .arg(format)
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(2));
            assert!(
                String::from_utf8_lossy(&result.stderr).contains("\"code\":\"resource-limit\"")
            );
            assert!(!refused.exists());
        }
    }
    assert_eq!(std::fs::read(input).unwrap(), [37; 512]);
    assert_eq!(directory.path().read_dir().unwrap().count(), 11);
}

#[cfg(unix)]
#[test]
fn closed_materialization_progress_pipe_removes_unpublished_staging() {
    let _process_boundary = subprocess_test();
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
    };
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("source");
    std::fs::write(&input, [7; 512]).unwrap();
    for command in ["convert", "compact"] {
        let output = directory.path().join(command);
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(receiver);
        let sender: OwnedFd = sender.into();
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--progress", command])
            .arg(&input)
            .arg("raw")
            .arg(&output)
            .arg("qcow2")
            .stdout(Stdio::piped())
            .stderr(Stdio::from(sender))
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(!output.exists());
        assert_eq!(directory.path().read_dir().unwrap().count(), 1);
    }
    assert_eq!(std::fs::read(input).unwrap(), [7; 512]);
}

#[cfg(target_os = "linux")]
#[test]
fn capabilities_report_current_read_and_write_handles_without_mutation() {
    let _process_boundary = subprocess_test();
    use virtdisk::{ImageFormat, ImageWriter};
    let directory = tempfile::tempdir().unwrap();
    for (format, name) in [
        (ImageFormat::Raw, "raw"),
        (ImageFormat::Qcow2, "qcow2"),
        (ImageFormat::Vhdx, "vhdx"),
        (ImageFormat::Vdi, "vdi"),
        (ImageFormat::Vmdk, "vmdk"),
    ] {
        let path = directory.path().join(name);
        drop(ImageWriter::create_sparse(&path, format, 65536).unwrap());
        let original = std::fs::read(&path).unwrap();
        for access in ["read", "write"] {
            let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(["capabilities", access])
                .arg(&path)
                .arg(name)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let report = String::from_utf8(output.stdout).unwrap();
            assert!(report.starts_with("{\"type\":\"capabilities\",\"scope\":\"opened-handle\","));
            assert_eq!(report.matches("\"operation\":").count(), 16);
            if access == "read" {
                assert!(report.contains("\"access\":\"read-only\""));
                assert!(report.contains(
                    "{\"operation\":\"write\",\"supported\":false,\"reason\":\"read-only-handle\"}"
                ));
            } else {
                assert!(report.contains("\"access\":\"read-write\""));
                assert!(
                    report.contains("{\"operation\":\"write\",\"supported\":true,\"reason\":null}")
                );
            }
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }
    assert_eq!(directory.path().read_dir().unwrap().count(), 5);
}

#[test]
fn capability_controls_are_validated_before_file_access() {
    let _process_boundary = subprocess_test();
    for arguments in [
        vec!["capabilities", "invalid", "absent", "raw"],
        vec![
            "--parser-limit",
            "metadata=1",
            "capabilities",
            "write",
            "absent",
            "raw",
        ],
        vec![
            "--replay-vhdx-log",
            "capabilities",
            "write",
            "absent",
            "vhdx",
        ],
        vec!["--recover", "capabilities", "write", "absent", "raw"],
        vec!["--progress", "capabilities", "read", "absent", "raw"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("No such file"));
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 512]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
        .args(["--parser-limit", "work=1", "capabilities", "read"])
        .arg(&path)
        .arg("raw")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(std::fs::read(path).unwrap(), [37; 512]);
}

#[cfg(target_os = "linux")]
#[test]
fn write_capabilities_reject_pending_recovery_and_held_locks() {
    let _process_boundary = subprocess_test();
    use virtdisk::{ImageFormat, ImageWriter};
    let directory = tempfile::tempdir().unwrap();
    for (format, name, suffix) in [
        (ImageFormat::Qcow2, "qcow2", ".virtdisk-qcow2-journal"),
        (ImageFormat::Vdi, "vdi", ".virtdisk-transaction"),
        (ImageFormat::Vmdk, "vmdk", ".virtdisk-transaction"),
    ] {
        let path = directory.path().join(name);
        let writer = ImageWriter::create(&path, format, 65536).unwrap();
        let before = std::fs::read(&path).unwrap();
        let locked = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["capabilities", "write"])
            .arg(&path)
            .arg(name)
            .output()
            .unwrap();
        assert_eq!(locked.status.code(), Some(2));
        assert!(locked.stdout.is_empty());
        drop(writer);
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = std::path::PathBuf::from(sidecar);
        std::fs::write(&sidecar, b"pending evidence").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--json-errors", "capabilities", "write"])
            .arg(&path)
            .arg(name)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("\"code\":\"recovery-required\""));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(sidecar).unwrap(), b"pending evidence");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn capabilities_keep_parent_authorization_and_backed_resize_restrictions() {
    let _process_boundary = subprocess_test();
    use virtdisk::{ImageFormat, ImageWriter, VmdkWriter};
    let directory = tempfile::tempdir().unwrap();
    for (format, name) in [
        (ImageFormat::Qcow2, "qcow2"),
        (ImageFormat::Vhdx, "vhdx"),
        (ImageFormat::Vdi, "vdi"),
        (ImageFormat::Vmdk, "vmdk"),
    ] {
        let base = directory.path().join(format!("base-{name}"));
        let child = directory.path().join(format!("child-{name}"));
        drop(ImageWriter::create_sparse(&base, format, 65536).unwrap());
        match format {
            ImageFormat::Qcow2 => {
                virtdisk::create_qcow2_overlay(&child, &base, "qcow2", 65536).unwrap()
            }
            ImageFormat::Vhdx => virtdisk::create_vhdx_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Vdi => virtdisk::create_vdi_overlay(&child, &base, &[]).unwrap(),
            ImageFormat::Vmdk => drop(
                VmdkWriter::create_overlay(&child, &base, std::slice::from_ref(&base)).unwrap(),
            ),
            ImageFormat::Raw => unreachable!(),
        }
        let original_base = std::fs::read(&base).unwrap();
        let original_child = std::fs::read(&child).unwrap();
        for access in ["read", "write"] {
            let denied = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(["capabilities", access])
                .arg(&child)
                .arg(name)
                .output()
                .unwrap();
            assert_eq!(denied.status.code(), Some(2));
            assert!(denied.stdout.is_empty());
            let allowed = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
                .args(["capabilities", access])
                .arg(&child)
                .arg(name)
                .arg(&base)
                .output()
                .unwrap();
            assert!(
                allowed.status.success(),
                "{}",
                String::from_utf8_lossy(&allowed.stderr)
            );
            let report = String::from_utf8(allowed.stdout).unwrap();
            assert!(report.contains("\"has_parent\":true"));
            let resize_supported =
                access == "write" && format == ImageFormat::Qcow2 && cfg!(target_os = "linux");
            assert!(report.contains(&format!(
                "{{\"operation\":\"resize\",\"supported\":{resize_supported},"
            )));
        }
        assert_eq!(std::fs::read(base).unwrap(), original_base);
        assert_eq!(std::fs::read(child).unwrap(), original_child);
    }
}

#[cfg(unix)]
#[test]
fn closed_capability_output_pipe_returns_an_error_without_mutation() {
    let _process_boundary = subprocess_test();
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
    };
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw");
    std::fs::write(&path, [37; 512]).unwrap();
    for access in ["read", "write"] {
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(receiver);
        let sender: OwnedFd = sender.into();
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--json-errors", "capabilities", access])
            .arg(&path)
            .arg("raw")
            .stdout(Stdio::from(sender))
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("\"kind\":\"broken-pipe\""));
        assert!(!error.contains("panicked"));
        assert_eq!(std::fs::read(&path).unwrap(), [37; 512]);
        let writer = virtdisk::RawWriter::open(&path).unwrap();
        drop(writer);
    }
}

#[test]
fn resize_progress_and_shared_quotas_cover_zero_tail_and_materialization() {
    let _process_boundary = subprocess_test();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("source");
    let mut bytes = vec![0; 1024];
    bytes[..512].fill(37);
    std::fs::write(&input, &bytes).unwrap();
    for format in ["raw", "qcow2", "vhdx", "vdi", "vmdk"] {
        let output = directory.path().join(format!("output-{format}"));
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--progress", "resize"])
            .arg(&input)
            .arg("raw")
            .arg(&output)
            .args([format, "512", "zero-tail"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let progress = String::from_utf8(result.stderr).unwrap();
        assert!(progress.contains("\"phase\":\"tail-validation\""));
        assert!(
            progress
                .lines()
                .last()
                .unwrap()
                .contains("\"phase\":\"publication\"")
        );
        let refused = directory.path().join(format!("refused-{format}"));
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--json-errors", "--operation-limit", "bytes=512", "resize"])
            .arg(&input)
            .arg("raw")
            .arg(&refused)
            .args([format, "512", "zero-tail"])
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&result.stderr).contains("\"code\":\"resource-limit\""));
        assert!(!refused.exists());
    }
    assert_eq!(std::fs::read(input).unwrap(), bytes);
    assert_eq!(directory.path().read_dir().unwrap().count(), 6);
}

#[cfg(target_os = "linux")]
#[test]
fn controlled_zeroing_reports_native_chunks_and_refuses_quota_before_mutation() {
    let _process_boundary = subprocess_test();
    use virtdisk::{ImageFormat, ImageWriter, WriteAt};
    let directory = tempfile::tempdir().unwrap();
    for (format, name) in [
        (ImageFormat::Raw, "raw"),
        (ImageFormat::Qcow2, "qcow2"),
        (ImageFormat::Vhdx, "vhdx"),
        (ImageFormat::Vdi, "vdi"),
        (ImageFormat::Vmdk, "vmdk"),
    ] {
        let path = directory.path().join(name);
        let writer = ImageWriter::create(&path, format, 131584).unwrap();
        writer.write_all_at(0, &vec![37; 131584]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let original = std::fs::read(&path).unwrap();
        let refused = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--json-errors", "--operation-limit", "io=1", "zero"])
            .arg(&path)
            .args([name, "7", "131073"])
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("\"code\":\"resource-limit\""));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let result = Command::new(env!("CARGO_BIN_EXE_virtdisk"))
            .args(["--progress", "--operation-limit", "scratch=1", "zero"])
            .arg(&path)
            .args([name, "7", "131073"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("\"phase\":\"zeroing\""));
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .lines()
                .last()
                .unwrap()
                .contains("\"io_operations\":3")
        );
        let writer = ImageWriter::open(&path, format).unwrap();
        let mut bytes = vec![0; 131584];
        writer.read_exact_at(0, &mut bytes).unwrap();
        assert!(
            bytes[..7]
                .iter()
                .chain(bytes[131080..].iter())
                .all(|&b| b == 37)
        );
        assert!(bytes[7..131080].iter().all(|&b| b == 0));
    }
}
