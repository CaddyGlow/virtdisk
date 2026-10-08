//! Bounded Rust native VHDX mutations on exclusively owned acceptance copies.
use crate::{Manifest, Mode, hash_file};
use sha2::{Digest, Sha256};
use std::{io, path::Path};
use virtdisk::{ReadAt, Vhdx, VhdxWriter};
pub struct Case {
    pub image: String,
    pub parents: Vec<String>,
    pub expected: Vec<u8>,
    pub logical_sector: u32,
}
fn input_model(m: &Manifest) -> io::Result<Vec<u8>> {
    if m.mode != Mode::Clean
        || m.capacity != 4 << 20
        || !matches!(m.logical_sector, 512 | 4096)
        || m.physical_sector != 4096
    {
        return Err(io::Error::other(
            "mutation requires known clean4MiB fixture",
        ));
    }
    let mut expected = vec![7; 4 << 20];
    match (m.image.as_str(), m.parents.as_slice()) {
        ("base.vhdx", []) => {}
        ("child.vhdx", [parent]) if parent == "base.vhdx" => {
            let sector = m.logical_sector as usize;
            expected[sector - 3..sector + 5].fill(9);
            expected[(1 << 20) + 17..(1 << 20) + 20].fill(8);
            expected[2 << 20..3 << 20].fill(0);
            expected[(2 << 20) + 3] = 6;
            expected[(3 << 20) + 1..(3 << 20) + 6].fill(0);
        }
        _ => {
            return Err(io::Error::other(
                "mutation fixture identity/profile mismatch",
            ));
        }
    }
    if format!("{:x}", Sha256::digest(&expected)) != m.logical_sha256 {
        return Err(io::Error::other("mutation independent model SHA mismatch"));
    }
    Ok(expected)
}
fn verify(root: &Path, case: &Case) -> io::Result<()> {
    let view = Vhdx::open_chain(
        root.join(&case.image),
        &case
            .parents
            .iter()
            .map(|p| root.join(p))
            .collect::<Vec<_>>(),
    )?;
    let mut bytes = vec![0; case.expected.len()];
    view.read_exact_at(0, &mut bytes)?;
    if bytes != case.expected {
        return Err(io::Error::other("Rust mutation fullmodel mismatch"));
    }
    Ok(())
}
fn writes(writer: &VhdxWriter, model: &mut [u8], sector: u32) -> io::Result<()> {
    for (offset, bytes) in [
        (u64::from(sector) - 2, vec![19; 7]),
        (8195, vec![11; 8]),
        ((1 << 20) + 8192, vec![13; 11]),
    ] {
        writer.write_all_at(offset, &bytes)?;
        model[offset as usize..offset as usize + bytes.len()].copy_from_slice(&bytes);
    }
    writer.write_zeroes((3 << 20) + 128, 8)?;
    model[(3 << 20) + 128..(3 << 20) + 136].fill(0);
    writer.flush()
}
fn guarded(writer: &mut VhdxWriter) -> io::Result<()> {
    #[cfg(windows)]
    {
        use virtdisk::{DiscardPolicy, ShrinkPolicy};
        for result in [
            writer
                .discard(0, 1 << 20, DiscardPolicy::RequireDeallocation)
                .map(|_| ()),
            writer.resize(5 << 20, ShrinkPolicy::Reject),
        ] {
            if !matches!(result, Err(ref error) if error.kind() == io::ErrorKind::Unsupported) {
                return Err(io::Error::other(
                    "Windows management capability guard mismatch",
                ));
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = writer;
    }
    Ok(())
}
/// Run steady-state mutations only; process/power-loss interruption is separate.
pub fn exercise(root: &Path, m: &Manifest) -> io::Result<Vec<Case>> {
    let mut expected = input_model(m)?;
    let parents: Vec<_> = m.parents.iter().map(|p| root.join(p)).collect();
    let image = root.join(&m.image);
    let initial = Case {
        image: m.image.clone(),
        parents: m.parents.clone(),
        expected: expected.clone(),
        logical_sector: m.logical_sector,
    };
    verify(root, &initial)?;
    let parent_before = if parents.is_empty() {
        None
    } else {
        Some(hash_file(&parents[0])?)
    };
    let unchanged = hash_file(&image)?;
    let mut writer = VhdxWriter::open_chain(&image, &parents)?;
    if VhdxWriter::open_chain(&image, &parents).is_ok() {
        return Err(io::Error::other(
            "competing writer unexpectedly acquired lock",
        ));
    }
    guarded(&mut writer)?;
    drop(writer);
    if unchanged != hash_file(&image)? {
        return Err(io::Error::other("unsupported management changed image"));
    }
    let writer = VhdxWriter::open_chain(&image, &parents)?;
    writes(&writer, &mut expected, m.logical_sector)?;
    drop(writer);
    let edited = Case {
        expected: expected.clone(),
        ..initial
    };
    verify(root, &edited)?;
    if let Some(before) = parent_before
        && before != hash_file(&parents[0])?
    {
        return Err(io::Error::other("Rust mutations changed authorized parent"));
    }
    let blank_path = root.join("rust-created.vhdx");
    let blank_writer = VhdxWriter::create(&blank_path, 4 << 20)?;
    if VhdxWriter::open(&blank_path).is_ok() {
        return Err(io::Error::other("created image competing lock succeeded"));
    }
    let mut blank_expected = vec![0; 4 << 20];
    let mut empty = vec![255; 4 << 20];
    blank_writer.read_exact_at(0, &mut empty)?;
    if empty != blank_expected {
        return Err(io::Error::other("Rust sparse creation was not zero"));
    }
    writes(&blank_writer, &mut blank_expected, 512)?;
    drop(blank_writer);
    let blank = Case {
        image: "rust-created.vhdx".into(),
        parents: vec![],
        expected: blank_expected,
        logical_sector: 512,
    };
    verify(root, &blank)?;
    let parent_model = if m.image == "base.vhdx" {
        expected
    } else {
        vec![7; 4 << 20]
    };
    let overlay_path = root.join("rust-overlay.vhdx");
    let overlay_writer = VhdxWriter::create_overlay(&overlay_path, root.join("base.vhdx"), &[])?;
    if VhdxWriter::open_chain(&overlay_path, &[root.join("base.vhdx")]).is_ok() {
        return Err(io::Error::other("created overlay competing lock succeeded"));
    }
    let mut overlay_expected = parent_model;
    writes(&overlay_writer, &mut overlay_expected, m.logical_sector)?;
    drop(overlay_writer);
    let overlay = Case {
        image: "rust-overlay.vhdx".into(),
        parents: vec!["base.vhdx".into()],
        expected: overlay_expected,
        logical_sector: m.logical_sector,
    };
    verify(root, &overlay)?;
    Ok(vec![edited, blank, overlay])
}
