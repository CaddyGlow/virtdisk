//! Read-only disk parsing with input, read, and validation work bounds.
use std::{io, sync::Arc};
use virtdisk::{Qcow2, ReadAt};

struct Bytes(Vec<u8>);
impl ReadAt for Bytes {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let end = start
            .checked_add(output.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "fuzz range overflow"))?;
        let source = self
            .0
            .get(start..end)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "fuzz source exhausted"))?;
        output.copy_from_slice(source);
        Ok(())
    }
}

/// Exercise bounded QCOW2 headers, mapping, compressed reads, and partitions.
/// Embedded backing names are never authorized or opened by this harness.
pub fn qcow2(data: &[u8]) {
    if data.len() > 1 << 20 {
        return;
    }
    let source: Arc<dyn ReadAt> = Arc::new(Bytes(data.to_vec()));
    let Ok(image) = Qcow2::open(source) else {
        return;
    };
    let mut checkpoints = 0;
    let _ = image.validate_active_mapping_with_cancel(|| {
        checkpoints += 1;
        checkpoints > 32
    });
    let mut output = [0u8; 4096];
    for start in [
        0,
        511,
        4095,
        image.len() / 2,
        image.len().saturating_sub(4096),
    ] {
        let count = image.len().saturating_sub(start).min(output.len() as u64) as usize;
        let _ = image.read_exact_at(start, &mut output[..count]);
    }
}

pub fn run(target: &str, data: &[u8]) -> Result<(), &'static str> {
    match target {
        "qcow2" => qcow2(data),
        _ => return Err("unknown fuzz target"),
    }
    Ok(())
}

pub const TARGETS: &[&str] = &["qcow2"];
pub fn seeds(_: &str) -> Vec<Vec<u8>> {
    let mut image = vec![0; 4096];
    image[..4].copy_from_slice(b"QFI\xfb");
    for (offset, value) in [(4, 3u32), (20, 9), (36, 1), (56, 1), (96, 4), (100, 104)] {
        image[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    for (offset, value) in [
        (24, 1536u64),
        (40, 512),
        (48, 1024),
        (512, 1536),
        (1536, 2048),
    ] {
        image[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
    }
    image[2048..2560].fill(0x5a);
    let accepted: Arc<dyn ReadAt> = Arc::new(Bytes(image.clone()));
    assert!(
        Qcow2::open(accepted).is_ok(),
        "valid QCOW2 seed must reach mapped reader"
    );
    vec![image, vec![0; 512], vec![]]
}

#[cfg(test)]
mod smoke {
    #[test]
    fn corpus_and_truncations_exercise_owned_harnesses() {
        for target in super::TARGETS {
            for seed in super::seeds(target) {
                super::run(target, &seed).unwrap();
                for end in [0, seed.len() / 2, seed.len().saturating_sub(1)] {
                    super::run(target, &seed[..end]).unwrap();
                }
                for offset in (0..seed.len()).step_by((seed.len() / 16).max(1)) {
                    let mut mutation = seed.clone();
                    mutation[offset] ^= 0xff;
                    super::run(target, &mutation).unwrap();
                }
            }
        }
    }
}
