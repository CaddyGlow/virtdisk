#![cfg(feature = "std")]
//! Independent native VirtualBox format acceptance; no VM execution is involved.
use std::{ffi::OsStr, fs, path::Path, process::Command, sync::Arc};
use virtdisk::io;
use virtdisk::{RawDisk, ReadAt, Vdi, VdiWriter, create_vdi, create_vdi_overlay};

// VBoxSVC and its temporary registry must not race another test in this binary.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Oracle {
    directory: tempfile::TempDir,
    ipc_id: String,
}
impl Oracle {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("registry")).unwrap();
        // VirtualBox's IPC socket selection is independent of VBOX_USER_HOME.
        // See upstream src/libs/xpcom18a4/ipc/ipcd/shared/src/ipcConfig.cpp.
        let ipc_id = format!(
            "virtdisk-{}-{}",
            std::process::id(),
            directory.path().file_name().unwrap().to_string_lossy()
        );
        let oracle = Self { directory, ipc_id };
        let version = oracle.run(["--version"]);
        eprintln!("native VirtualBox oracle: {}", version.trim());
        let inventory = oracle.run(["list", "hdds"]);
        assert!(
            inventory.trim().is_empty(),
            "temporary registry is not isolated: {inventory}"
        );
        oracle
    }
    fn path(&self, name: &str) -> std::path::PathBuf {
        self.directory.path().join(name)
    }
    fn run<I, S>(&self, args: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new("VBoxManage")
            .env("VBOX_USER_HOME", self.path("registry"))
            .env("VBOX_IPC_SOCKETID", &self.ipc_id)
            .args(args)
            .output()
            .expect("VBoxManage must be installed in PATH");
        assert!(
            output.status.success(),
            "VBoxManage failed: status={}\nstdout={}\nstderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn convert(&self, raw: &Path, output: &Path, variant: &str) {
        self.run([
            OsStr::new("convertfromraw"),
            raw.as_os_str(),
            output.as_os_str(),
            OsStr::new("--format"),
            OsStr::new("VDI"),
            OsStr::new("--variant"),
            OsStr::new(variant),
        ]);
    }
    fn flatten(&self, source: &Path, output: &Path) -> Vec<u8> {
        self.run([
            OsStr::new("clonemedium"),
            OsStr::new("disk"),
            source.as_os_str(),
            output.as_os_str(),
            OsStr::new("--format"),
            OsStr::new("RAW"),
        ]);
        fs::read(output).unwrap()
    }
    fn inspect(&self, path: &Path) -> String {
        self.run([
            OsStr::new("showmediuminfo"),
            OsStr::new("disk"),
            path.as_os_str(),
        ])
    }
}
struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_exact_at(&self, at: u64, out: &mut [u8]) -> io::Result<()> {
        let range = self
            .0
            .get(at as usize..at as usize + out.len())
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        out.copy_from_slice(range);
        Ok(())
    }
}
fn pattern() -> Vec<u8> {
    let mut bytes = vec![0; 3 * 1048576];
    for (index, byte) in bytes.iter_mut().enumerate() {
        if index / 1048576 != 1 {
            *byte = (index % 251 + 1) as u8;
        }
    }
    bytes
}
fn read_disk(disk: &dyn ReadAt) -> Vec<u8> {
    let mut bytes = vec![0; disk.len() as usize];
    disk.read_exact_at(0, &mut bytes).unwrap();
    bytes
}
fn cow(writer: &VdiWriter, expected: &mut [u8]) {
    writer.write_all_at(1048570, &[77; 20]).unwrap();
    expected[1048570..1048590].fill(77);
    writer.write_zeroes(2 * 1048576 + 17, 31).unwrap();
    expected[2 * 1048576 + 17..2 * 1048576 + 48].fill(0);
    writer.flush().unwrap();
    let mut observed = vec![0; expected.len()];
    writer.read_exact_at(0, &mut observed).unwrap();
    assert_eq!(observed, expected);
}

#[test]
#[ignore = "requires native VBoxManage; isolated temporary VirtualBox registry"]
fn native_fixed_dynamic_and_differencing_fixtures_read_and_cow() {
    let _serial = SERIAL.lock().unwrap();
    let oracle = Oracle::new();
    let original = pattern();
    let raw = oracle.path("source.raw");
    fs::write(&raw, &original).unwrap();
    for variant in ["Fixed", "Standard"] {
        let base = oracle.path(&format!("base-{variant}.vdi"));
        let child = oracle.path(&format!("native-child-{variant}.vdi"));
        oracle.convert(&raw, &base, variant);
        let base_bytes = fs::read(&base).unwrap();
        assert_eq!(
            u32::from_le_bytes(base_bytes[468..472].try_into().unwrap()),
            512
        );
        let reader = Vdi::open(Arc::new(RawDisk::open(&base).unwrap())).unwrap();
        assert_eq!(read_disk(&reader), original);
        oracle.run([
            OsStr::new("createmedium"),
            OsStr::new("disk"),
            OsStr::new("--filename"),
            child.as_os_str(),
            OsStr::new("--diffparent"),
            base.as_os_str(),
            OsStr::new("--format"),
            OsStr::new("VDI"),
        ]);
        let child_header = fs::read(&child).unwrap();
        assert_eq!(&child_header[424..440], &base_bytes[392..408]);
        assert_eq!(&child_header[440..456], &base_bytes[408..424]);
        let reader = Vdi::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        assert_eq!(read_disk(&reader), original);
        drop(reader);
        let writer = VdiWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
        let mut expected = original.clone();
        cow(&writer, &mut expected);
        drop(writer);
        let changed = fs::read(&child).unwrap();
        assert_eq!(&changed[392..408], &child_header[392..408]);
        assert_ne!(&changed[408..424], &child_header[408..424]);
        assert_eq!(fs::read(&base).unwrap(), base_bytes);
        assert_eq!(
            oracle.flatten(&child, &oracle.path(&format!("native-flat-{variant}.raw"))),
            expected
        );
    }
}

#[test]
#[ignore = "requires native VBoxManage; isolated temporary VirtualBox registry"]
fn native_virtualbox_accepts_exports_and_native_parented_children() {
    let _serial = SERIAL.lock().unwrap();
    let oracle = Oracle::new();
    let original = pattern();
    let base = oracle.path("export.vdi");
    create_vdi(&base, &Bytes(original.clone())).unwrap();
    let info = oracle.inspect(&base);
    assert!(info.contains("VDI"), "{info}");
    assert_eq!(oracle.flatten(&base, &oracle.path("export.raw")), original);
    let before = fs::read(&base).unwrap();
    let child = oracle.path("export-child.vdi");
    create_vdi_overlay(&child, &base, &[]).unwrap();
    let header = fs::read(&child).unwrap();
    assert_eq!(&header[424..440], &before[392..408]);
    assert_eq!(&header[440..456], &before[408..424]);
    assert_ne!(&header[392..408], &before[392..408]);
    oracle.inspect(&child);
    assert_eq!(
        oracle.flatten(&child, &oracle.path("empty-child.raw")),
        original
    );
    let writer = VdiWriter::open_chain(&child, std::slice::from_ref(&base)).unwrap();
    let mut expected = original.clone();
    cow(&writer, &mut expected);
    drop(writer);
    assert_eq!(
        oracle.flatten(&child, &oracle.path("written-child.raw")),
        expected
    );
    assert_eq!(fs::read(&base).unwrap(), before);
    let writer_child = oracle.path("writer-child.vdi");
    let writer = VdiWriter::create_overlay(&writer_child, &base, &[]).unwrap();
    let mut expected = original.clone();
    cow(&writer, &mut expected);
    drop(writer);
    oracle.inspect(&writer_child);
    assert_eq!(
        oracle.flatten(&writer_child, &oracle.path("writer-child.raw")),
        expected
    );
    let fixed = oracle.path("writer-fixed.vdi");
    let writer = VdiWriter::create(&fixed, original.len() as u64).unwrap();
    writer.write_all_at(0, &original).unwrap();
    writer.flush().unwrap();
    drop(writer);
    oracle.inspect(&fixed);
    assert_eq!(
        oracle.flatten(&fixed, &oracle.path("fixed-export.raw")),
        original
    );
    let sparse = oracle.path("writer-sparse.vdi");
    let writer = VdiWriter::create_sparse(&sparse, original.len() as u64).unwrap();
    writer.write_all_at(0, &original[..1048576]).unwrap();
    writer
        .write_all_at(2 * 1048576, &original[2 * 1048576..])
        .unwrap();
    writer.flush().unwrap();
    drop(writer);
    oracle.inspect(&sparse);
    assert_eq!(
        oracle.flatten(&sparse, &oracle.path("sparse-export.raw")),
        original
    );
}
