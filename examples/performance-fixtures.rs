//! Generate private, verified streaming conversion outputs for native acceptance.
use std::{fs::OpenOptions, io::Write, path::PathBuf};
use virtdisk::{
    ConversionPolicy, Image, ImageFormat, OperationContext, OperationEvent, OperationLimits,
    OperationProfiler, convert_image_with_policy_and_context,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: performance-fixtures NEW_DIRECTORY")?,
    );
    std::fs::create_dir(&root)?;
    for sparse in [false, true] {
        let pattern = if sparse { "sparse" } else { "dense" };
        let raw = root.join(format!("{pattern}.raw"));
        let mut file = OpenOptions::new().create_new(true).write(true).open(&raw)?;
        for block in 0..65 {
            let length = if block == 64 { 512 } else { 65536 };
            let bytes = vec![
                if sparse && block % 16 != 0 {
                    0
                } else {
                    (block + 1) as u8
                };
                length
            ];
            file.write_all(&bytes)?;
        }
        file.sync_all()?;
        drop(file);
        let image = Image::open(&raw, Some(ImageFormat::Raw))?;
        for (name, format) in [("vdi", ImageFormat::Vdi), ("vhdx", ImageFormat::Vhdx)] {
            let profiler = OperationProfiler::default();
            let mut context =
                OperationContext::new(OperationLimits::default().working_memory_bytes(4 << 20)?)
                    .with_streaming_allocation(true)
                    .with_profiler(profiler.clone());
            convert_image_with_policy_and_context(
                &image,
                root.join(format!("{pattern}.{name}")),
                format,
                ConversionPolicy::Default,
                &mut context,
            )?;
            assert_eq!(profiler.metrics(OperationEvent::AllocationRead).bytes, 0);
            println!("{pattern}.{name}: streaming allocation verified");
        }
    }
    Ok(())
}
