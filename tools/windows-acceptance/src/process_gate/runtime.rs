//! Windows controller/worker for acknowledged real flush boundaries.
use super::{
    Ack, Policy, Workload,
    windows::{self, Hook},
};
use crate::{FileIdentity, Manifest, Mode, hash_file};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use virtdisk::{ReadAt, Vhdx, VhdxWriter};
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    manifest: Manifest,
    workload: Workload,
    intercept: bool,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    if std::fs::metadata(path)?.len() > 65536 {
        return Err(io::Error::other("configuration exceeds64KiB"));
    }
    serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)
}
fn verify_view(view: &Vhdx, expected: &[u8]) -> io::Result<()> {
    let mut bytes = vec![0; expected.len()];
    view.read_exact_at(0, &mut bytes)?;
    if bytes != expected {
        return Err(io::Error::other(
            "full independent process-gate model mismatch",
        ));
    }
    Ok(())
}
fn send(ack: Ack) -> io::Result<()> {
    let mut output = std::io::stdout().lock();
    output.write_all(&ack.encode())?;
    output.flush()
}
fn go() -> io::Result<()> {
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte)?;
    if byte != [1] {
        return Err(io::Error::other("invalid controller continuation"));
    }
    Ok(())
}
pub fn worker(config: &Path) -> io::Result<()> {
    let c: Config = read_json(config)?;
    let root = config
        .parent()
        .ok_or_else(|| io::Error::other("missing worker directory"))?;
    if c.manifest.image != "target.vhdx"
        || c.manifest.mode != Mode::Clean
        || c.manifest.capacity != 4 << 20
        || c.manifest.logical_sector != c.workload.sector
        || c.manifest.physical_sector != 4096
        || c.manifest.parents
            != if c.workload.child {
                vec!["base.vhdx".to_owned()]
            } else {
                vec![]
            }
    {
        return Err(io::Error::other("worker authorization profile mismatch"));
    }
    crate::validate(root, &c.manifest)?;
    crate::strict_locators(root, &c.manifest)?;
    let fresh = Workload {
        retained: false,
        ..c.workload
    }
    .models()?
    .0;
    if c.manifest.logical_sha256 != hash(&fresh) {
        return Err(io::Error::other("worker input model hash mismatch"));
    }
    verify_view(&crate::authorized_view(root, &c.manifest)?, &fresh)?;
    let path = root.join(&c.manifest.image);
    let id = windows::identity(&path)?;
    let parents: Vec<_> = c.manifest.parents.iter().map(|p| root.join(p)).collect();
    let writer = VhdxWriter::open_chain(&path, &parents)?;
    if c.workload.retained {
        writer.write_all_at(8195, &[11; 8])?;
        writer.flush()?;
        let mut bytes = vec![0; 4 << 20];
        writer.read_exact_at(0, &mut bytes)?;
        if bytes != c.workload.models()?.0 {
            return Err(io::Error::other("retained baseline mismatch"));
        }
    }
    let hook = if c.intercept {
        Some(Hook::install(id)?)
    } else {
        None
    };
    send(Ack::ready(id))?;
    go()?;
    let (offset, bytes) = c.workload.write();
    writer.write_all_at(offset, &bytes)?;
    writer.flush()?;
    let count = if let Some(h) = hook { h.finish()? } else { 0 };
    drop(writer);
    send(Ack::done(count, id))?;
    Ok(())
}
fn manifest(root: &Path, w: Workload, sha: String, mode: Mode) -> io::Result<Manifest> {
    let parents = if w.child {
        vec!["base.vhdx".to_owned()]
    } else {
        vec![]
    };
    let mut files = Vec::new();
    for name in std::iter::once("target.vhdx").chain(parents.iter().map(String::as_str)) {
        let p = root.join(name);
        files.push(FileIdentity {
            path: name.into(),
            length: std::fs::metadata(&p)?.len(),
            sha256: hash_file(&p)?,
        });
    }
    Ok(Manifest {
        version: 1,
        image: "target.vhdx".into(),
        parents,
        files,
        capacity: 4 << 20,
        logical_sector: w.sector,
        physical_sector: 4096,
        logical_sha256: sha,
        mode,
    })
}
fn prepare(root: &Path, base: &Path, w: Workload) -> io::Result<Manifest> {
    std::fs::create_dir(root)?;
    let target = root.join("target.vhdx");
    if w.child {
        std::fs::copy(base, root.join("base.vhdx"))?;
        drop(VhdxWriter::create_overlay(
            &target,
            root.join("base.vhdx"),
            &[],
        )?);
    } else {
        drop(VhdxWriter::create(&target, 4 << 20)?);
        if w.sector == 4096 {
            // Geometry fixture preparation precedes worker start and interception.
            use std::io::{Seek, SeekFrom};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&target)?;
            let mut offset = [0; 8];
            f.seek(SeekFrom::Start(196672))?;
            f.read_exact(&mut offset)?;
            f.seek(SeekFrom::Start(u64::from_le_bytes(offset) + 65584))?;
            f.write_all(&4096u32.to_le_bytes())?;
            f.sync_all()?;
        }
    }
    let m = manifest(
        root,
        w,
        hash(
            &Workload {
                retained: false,
                ..w
            }
            .models()?
            .0,
        ),
        Mode::Clean,
    )?;
    crate::validate(root, &m)?;
    verify_view(
        &crate::authorized_view(root, &m)?,
        &Workload {
            retained: false,
            ..w
        }
        .models()?
        .0,
    )?;
    Ok(m)
}
struct Running(Option<Child>);
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn run_worker(
    root: &Path,
    m: Manifest,
    w: Workload,
    intercept: bool,
    cut: Option<u32>,
) -> io::Result<serde_json::Value> {
    let path = root.join("worker.json");
    let config = Config {
        manifest: m,
        workload: w,
        intercept,
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    let id = windows::identity(&root.join("target.vhdx"))?;
    let child = Command::new(std::env::current_exe()?)
        .arg("--worker")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let mut running = Running(Some(child));
    let child = running.0.as_mut().unwrap();
    let pid = child.id();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::sync_channel(2);
    let reader = std::thread::spawn(move || {
        loop {
            let mut b = [0; 32];
            let result = output.read_exact(&mut b).and_then(|_| Ack::decode(&b));
            let failed = result.is_err();
            if tx.send(result).is_err() || failed {
                break;
            }
        }
    });
    let mut policy = Policy::new(id);
    let mut killed = false;
    let mut acknowledgements = Vec::new();
    loop {
        let ack = rx
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| io::Error::other("worker flush gate timeout"))??;
        policy.observe(ack)?;
        acknowledgements.push(serde_json::json!({"kind":ack.kind,"cut":ack.cut,"volume":ack.identity.volume,"file":ack.identity.file}));
        if cut == Some(ack.cut) && ack.kind != 3 {
            windows::terminate(child)?;
            killed = true;
            break;
        }
        if ack.kind == 3 {
            break;
        }
        input.write_all(&[1])?;
    }
    drop(input);
    let status = child.wait()?;
    running.0.take();
    drop(rx);
    let _ = reader.join();
    if killed && status.code() != Some(0x56444355) {
        return Err(io::Error::other(
            "worker did not exit with actual termination code",
        ));
    }
    if !killed && !status.success() {
        return Err(io::Error::other("worker failed control/trace"));
    }
    if cut.is_some() && !killed {
        return Err(io::Error::other("requested cut was not observed"));
    }
    Ok(
        serde_json::json!({"pid":pid,"intercepted":intercept,"cut":cut,"terminate_process":killed,"exit_code":status.code(),"acknowledgements":acknowledgements,"successful_flushes":policy.cuts(),"leaf_sha256":hash_file(&root.join("target.vhdx"))?}),
    )
}
fn check_result(root: &Path, w: Workload, killed: bool) -> io::Result<serde_json::Value> {
    let (old, new) = w.models()?;
    let raw = manifest(
        root,
        w,
        hash(&new),
        if killed {
            Mode::NativeReplay
        } else {
            Mode::Clean
        },
    )?;
    let view = crate::authorized_view(root, &raw)?;
    let mut bytes = vec![0; 4 << 20];
    view.read_exact_at(0, &mut bytes)?;
    drop(view);
    let selected = if bytes == new {
        "new"
    } else if killed && bytes == old {
        "old"
    } else {
        return Err(io::Error::other(
            "interrupted fullimage outside independent old/new models",
        ));
    };
    let expected = hash(&bytes);
    let mut original = raw;
    original.logical_sha256 = expected.clone();
    let library = crate::copy_tree(root, &original)?;
    let parents: Vec<_> = original
        .parents
        .iter()
        .map(|p| library.path().join(p))
        .collect();
    virtdisk::recover_vhdx_chain(library.path().join("target.vhdx"), &parents)?;
    let clean = Manifest {
        mode: Mode::LibraryRecovery,
        ..manifest(library.path(), w, expected.clone(), Mode::LibraryRecovery)?
    };
    let clean_before = hash_file(&library.path().join("target.vhdx"))?;
    let library_native = crate::native::readout(&library.path().join("target.vhdx"), &clean)?;
    let clean_after = hash_file(&library.path().join("target.vhdx"))?;
    if clean_before != clean_after {
        return Err(io::Error::other(
            "native readout changed clean recovered leaf",
        ));
    }
    let native = crate::copy_tree(root, &original)?;
    let native_before = hash_file(&native.path().join("target.vhdx"))?;
    let native_result = crate::native::readout(&native.path().join("target.vhdx"), &original)?;
    let native_after = hash_file(&native.path().join("target.vhdx"))?;
    for copy in [library.path(), native.path()] {
        for p in &original.parents {
            if hash_file(&copy.join(p))?
                != original.files.iter().find(|f| f.path == *p).unwrap().sha256
            {
                return Err(io::Error::other("replay changed copied parent"));
            }
        }
    }
    // Released writer locks and clean reopen are separately required after recovery.
    drop(VhdxWriter::open_chain(
        library.path().join("target.vhdx"),
        &parents,
    )?);
    crate::validate(root, &original)?;
    Ok(
        serde_json::json!({"model":selected,"logical_sha256":expected,"raw_identities":original.files,"library_recovery_native":library_native,"native_replay":native_result,"released_writer_lock":true,"clean_recovered_before":clean_before,"clean_recovered_after":clean_after,"native_replay_before":native_before,"native_replay_after":native_after}),
    )
}
pub fn controller(manifest_path: &Path, output: &Path) -> io::Result<serde_json::Value> {
    let m: Manifest = read_json(manifest_path)?;
    crate::producer_plan(&m)?;
    let source = manifest_path
        .parent()
        .ok_or_else(|| io::Error::other("missing fixture root"))?;
    let copy = crate::copy_tree(source, &m)?;
    crate::strict_locators(copy.path(), &m)?;
    verify_view(&crate::authorized_view(copy.path(), &m)?, &vec![7; 4 << 20])?;
    std::fs::create_dir(output)?;
    let output = output.canonicalize()?;
    let mut results = Vec::new();
    let exe = hash_file(&std::env::current_exe()?)?;
    for child in [false, true] {
        for retained in [false, true] {
            let w = Workload {
                child,
                sector: m.logical_sector,
                retained,
            };
            let group = output.join(format!(
                "{}-{}",
                if child { "child" } else { "sparse" },
                if retained { "retained" } else { "fresh" }
            ));
            std::fs::create_dir(&group)?;
            let run = |name: String,
                       intercept: bool,
                       cut: Option<u32>|
             -> io::Result<serde_json::Value> {
                let root = group.join(&name);
                let initial = prepare(&root, &copy.path().join(&m.image), w)?;
                let parent_before = if child {
                    Some(hash_file(&root.join("base.vhdx"))?)
                } else {
                    None
                };
                let process = run_worker(&root, initial, w, intercept, cut)?;
                std::fs::write(
                    root.join("process.json"),
                    serde_json::to_vec_pretty(&process)?,
                )?;
                if let Some(before) = parent_before
                    && before != hash_file(&root.join("base.vhdx"))?
                {
                    return Err(io::Error::other("killed worker changed parent"));
                }
                let check = check_result(&root, w, cut.is_some())?;
                let receipt = serde_json::json!({"case":name,"workload":w,"process":process,"verification":check});
                std::fs::write(
                    root.join("receipt.json"),
                    serde_json::to_vec_pretty(&receipt)?,
                )?;
                Ok(receipt)
            };
            results.push(run("control".into(), false, None)?);
            let trace = run("trace".into(), true, None)?;
            let count = trace["process"]["successful_flushes"]
                .as_u64()
                .ok_or_else(|| io::Error::other("missing traced cut count"))?;
            if count == 0 || count > 128 {
                return Err(io::Error::other("unfulfilled bounded flush trace"));
            }
            results.push(trace);
            for cut in 0..=count as u32 {
                results.push(run(format!("cut-{cut}"), true, Some(cut))?);
                std::fs::write(
                    output.join("partial-receipt.json"),
                    serde_json::to_vec_pretty(&results)?,
                )?;
            }
        }
    }
    crate::validate(source, &m)?;
    Ok(
        serde_json::json!({"gate":"passed","executable_sha256":exe,"source_manifest_sha256":hash_file(manifest_path)?,"cases":results,"process_interruption":"instrumented_real_termination","power_loss":"unfulfilled"}),
    )
}
