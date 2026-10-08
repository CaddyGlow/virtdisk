use std::{io, path::Path};
use virtdisk_windows_acceptance::*;
fn run(manifest_path: &Path) -> io::Result<serde_json::Value> {
    let metadata = std::fs::metadata(manifest_path)?;
    if metadata.len() > 65536 {
        return Err(io::Error::other("manifest exceeds64KiB"));
    }
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    let copy = copy_tree(root, &m)?;
    // Authorization resolves and pins the complete chain before native APIs.
    drop(authorized_view(copy.path(), &m)?);
    strict_locators(copy.path(), &m)?;
    if m.mode != Mode::Clean {
        // Validate native replay's final locator state on an independent redo copy.
        let shadow = copy_tree(root, &m)?;
        let parents: Vec<_> = m.parents.iter().map(|p| shadow.path().join(p)).collect();
        virtdisk::recover_vhdx_chain(shadow.path().join(&m.image), &parents)?;
        strict_locators(shadow.path(), &m)?;
    }
    #[cfg(not(windows))]
    {
        Ok(
            serde_json::json!({"gate":"unfulfilled","reason":"native Windows runtime required","mode":m.mode,"source_identities":m.files}),
        )
    }
    #[cfg(windows)]
    {
        let parents: Vec<_> = m.parents.iter().map(|p| copy.path().join(p)).collect();
        if m.mode == Mode::LibraryRecovery {
            virtdisk::recover_vhdx_chain(copy.path().join(&m.image), &parents)?;
        }
        let before:Vec<_>=m.files.iter().map(|f|Ok(serde_json::json!({"path":f.path,"sha256":hash_file(&copy.path().join(&f.path))?}))).collect::<io::Result<_>>()?;
        let result = native::readout(&copy.path().join(&m.image), &m);
        let after:Vec<_>=m.files.iter().map(|f|Ok(serde_json::json!({"path":f.path,"sha256":hash_file(&copy.path().join(&f.path))?}))).collect::<io::Result<_>>()?;
        for (i, f) in m.files.iter().enumerate() {
            if (f.path != m.image || m.mode == Mode::Clean) && before[i] != after[i] {
                return Err(io::Error::other(
                    "parent or clean source changed during native acceptance",
                ));
            }
        }
        Ok(match result {
            Ok(native) => {
                serde_json::json!({"gate":"passed","mode":m.mode,"native":native,"before":before,"after":after})
            }
            Err(e) => {
                serde_json::json!({"gate":"failed","mode":m.mode,"error":e.to_string(),"before":before,"after":after})
            }
        })
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 2 {
        eprintln!("usage: virtdisk-windows-acceptance manifest.json (owned VHDX trees only)");
        std::process::exit(2)
    }
    let mut value = match run(Path::new(&args[1])) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"gate":"failed","error":e.to_string()}),
    };
    value["manifest_sha256"] = serde_json::json!(hash_file(Path::new(&args[1])).ok());
    #[cfg(windows)]
    {
        value["os_build"] = serde_json::json!(
            std::process::Command::new("cmd.exe")
                .args(["/c", "ver"])
                .output()
                .ok()
                .map(|v| String::from_utf8_lossy(&v.stdout).trim().to_owned())
        );
    }
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if value["gate"] != "passed" {
        std::process::exit(1)
    }
}
