use std::{io, path::Path};
use virtdisk_windows_acceptance::*;
fn run(manifest_path: &Path, output: &Path) -> io::Result<serde_json::Value> {
    if std::fs::metadata(manifest_path)?.len() > 65536 {
        return Err(io::Error::other("manifest exceeds64KiB"));
    }
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    let copy = copy_tree(root, &m)?;
    strict_locators(copy.path(), &m)?;
    #[cfg(not(windows))]
    {
        let _ = (copy, output);
        Ok(
            serde_json::json!({"gate":"unfulfilled","reason":"native Windows Rust mutation runtime required"}),
        )
    }
    #[cfg(windows)]
    {
        std::fs::create_dir(output)?;
        let output = output.canonicalize()?;
        for file in &m.files {
            std::fs::copy(copy.path().join(&file.path), output.join(&file.path))?;
        }
        let before: Vec<_> = m
            .files
            .iter()
            .map(|f| {
                Ok(serde_json::json!({"path":f.path,"sha256":hash_file(&output.join(&f.path))?}))
            })
            .collect::<io::Result<_>>()?;
        let cases = mutation::exercise(&output, &m)?;
        let mut results = Vec::new();
        for case in cases {
            use sha2::{Digest, Sha256};
            let check = Manifest {
                version: 1,
                image: case.image.clone(),
                parents: case.parents,
                files: vec![],
                capacity: case.expected.len() as u64,
                logical_sector: case.logical_sector,
                physical_sector: 4096,
                logical_sha256: format!("{:x}", Sha256::digest(case.expected)),
                mode: Mode::Clean,
            };
            let image = output.join(&case.image);
            let image_before = hash_file(&image)?;
            let native = native::readout(&image, &check)?;
            let image_after = hash_file(&image)?;
            if image_before != image_after {
                return Err(io::Error::other(
                    "native readout changed clean Rust-mutated image",
                ));
            }
            results.push(serde_json::json!({"image":case.image,"native":native,"before":image_before,"after":image_after,"rust_write_flush_reopen":"passed","competing_writer_lock":"passed"}));
        }
        let after: Vec<_> = m
            .files
            .iter()
            .map(|f| {
                Ok(serde_json::json!({"path":f.path,"sha256":hash_file(&output.join(&f.path))?}))
            })
            .collect::<io::Result<_>>()?;
        for (index, file) in m.files.iter().enumerate() {
            if file.path != m.image && before[index] != after[index] {
                return Err(io::Error::other("mutations changed copied parent"));
            }
        }
        validate(root, &m)?;
        Ok(
            serde_json::json!({"gate":"passed","cases":results,"before":before,"after":after,"source_manifest_sha256":hash_file(manifest_path)?,"native_discard":"unsupported_unchanged","native_resize":"unsupported_unchanged","process_interruption":"unfulfilled","power_loss":"unfulfilled"}),
        )
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        eprintln!("usage: mutate CLEAN_LIBRARY_FIXTURE_MANIFEST NEW_OUTPUT_DIRECTORY");
        std::process::exit(2);
    }
    let value = match run(Path::new(&args[1]), Path::new(&args[2])) {
        Ok(v) => v,
        Err(error) => serde_json::json!({"gate":"failed","error":error.to_string()}),
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if value["gate"] != "passed" {
        std::process::exit(1);
    }
}
