use std::{io, path::Path};
use virtdisk_windows_acceptance::*;
fn run(manifest_path: &Path, output: &Path) -> io::Result<serde_json::Value> {
    if std::fs::metadata(manifest_path)?.len() > 65536 {
        return Err(io::Error::other("manifest exceeds64KiB"));
    }
    let m: Manifest = serde_json::from_slice(&std::fs::read(manifest_path)?)?;
    producer_plan(&m)?;
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    let copy = copy_tree(root, &m)?;
    let view = authorized_view(copy.path(), &m)?;
    use virtdisk::ReadAt;
    let mut bytes = vec![0; 4 << 20];
    view.read_exact_at(0, &mut bytes)?;
    if bytes != vec![7; 4 << 20] {
        return Err(io::Error::other("parent model mismatch"));
    }
    drop(view);
    #[cfg(not(windows))]
    {
        let _ = output;
        Ok(serde_json::json!({"gate":"unfulfilled","reason":"native Windows production required"}))
    }
    #[cfg(windows)]
    {
        std::fs::create_dir(output)?;
        let output = output.canonicalize()?;
        let parent = output.join("base.vhdx");
        std::fs::copy(copy.path().join(&m.image), &parent)?;
        let before = hash_file(&parent)?;
        let native_result = native::produce_child(&parent, &output.join("child.vhdx"), &m);
        let after = hash_file(&parent)?;
        if before != after {
            return Err(io::Error::other("native production changed copied parent"));
        }
        validate(root, &m)?;
        let native = native_result?;
        Ok(
            serde_json::json!({"gate":"passed","native":native,"parent_before":before,"parent_after":after,"child_sha256":hash_file(&output.join("child.vhdx"))?,"source_manifest_sha256":hash_file(manifest_path)?,"source_identities":m.files}),
        )
    }
}
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        eprintln!("usage: produce CLEAN_STANDALONE_MANIFEST NEW_OUTPUT_DIRECTORY");
        std::process::exit(2);
    }
    let value = match run(Path::new(&args[1]), Path::new(&args[2])) {
        Ok(v) => v,
        Err(e) => serde_json::json!({"gate":"failed","error":e.to_string()}),
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if value["gate"] != "passed" {
        std::process::exit(1);
    }
}
