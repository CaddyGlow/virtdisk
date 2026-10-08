use std::path::Path;
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    #[cfg(windows)]
    if args.len() == 3 && args[1] == "--worker" {
        if let Err(error) =
            virtdisk_windows_acceptance::process_gate::runtime::worker(Path::new(&args[2]))
        {
            eprintln!("worker: {error}");
            std::process::exit(2);
        }
        return;
    }
    if args.len() != 3 {
        eprintln!("usage: process-kill CLEAN_BASE_MANIFEST NEW_OUTPUT_DIRECTORY");
        std::process::exit(2);
    }
    #[cfg(windows)]
    let value = match virtdisk_windows_acceptance::process_gate::runtime::controller(
        Path::new(&args[1]),
        Path::new(&args[2]),
    ) {
        Ok(value) => value,
        Err(error) => serde_json::json!({"gate":"failed","error":error.to_string()}),
    };
    #[cfg(not(windows))]
    let value = {
        let _ = Path::new(&args[1]);
        serde_json::json!({"gate":"unfulfilled","reason":"native Windows process termination runtime required"})
    };
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    if value["gate"] != "passed" {
        std::process::exit(1);
    }
}
