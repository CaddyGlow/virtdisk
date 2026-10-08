//! Read-only graph diagnostics with lossless native path bytes.
use super::json_output::JsonString;
use std::ffi::OsStr;
use virtdisk::io::{self, Write};
use virtdisk::{GraphManifest, ImageFormat, ImageGraph};

pub(super) fn write_json(
    writer: &mut impl Write,
    manifest: &GraphManifest,
    graph: &ImageGraph,
) -> io::Result<()> {
    // Complete graph reads before emitting a success record. Quota or identity
    // refusal must not produce partial success JSON.
    let mut sizes = Vec::new();
    sizes
        .try_reserve_exact(manifest.images().len())
        .map_err(io::Error::other)?;
    for image in manifest.images() {
        sizes.push(graph.reader(&image.path)?.len());
    }
    writer.write_all(b"{\"selected\":")?;
    index(
        writer,
        manifest.selected().and_then(|path| {
            manifest
                .images()
                .iter()
                .position(|image| image.path == path)
        }),
    )?;
    writer.write_all(b",\"images\":[")?;
    for (number, (image, size)) in manifest.images().iter().zip(sizes).enumerate() {
        if number != 0 {
            writer.write_all(b",")?;
        }
        write!(writer, "{{\"index\":{number},\"path_display\":")?;
        match image.path.to_str() {
            Some(path) => json_string(writer, path)?,
            None => writer.write_all(b"null")?,
        }
        write!(
            writer,
            ",\"path_encoding\":\"{}\",\"path_hex\":\"",
            path_encoding()
        )?;
        path_hex(writer, image.path.as_os_str())?;
        let format = match image.format {
            ImageFormat::Raw => "raw",
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Vhdx => "vhdx",
            ImageFormat::Vdi => "vdi",
            ImageFormat::Vmdk => "vmdk",
        };
        write!(writer, "\",\"format\":\"{format}\",\"parent\":")?;
        index(
            writer,
            image.parent.as_ref().and_then(|path| {
                manifest
                    .images()
                    .iter()
                    .position(|image| &image.path == path)
            }),
        )?;
        write!(writer, ",\"virtual_size\":{size}}}")?;
    }
    Ok(writer.write_all(b"]}\n")?)
}

fn index(writer: &mut impl Write, value: Option<usize>) -> io::Result<()> {
    match value {
        Some(value) => Ok(write!(writer, "{value}")?),
        None => Ok(writer.write_all(b"null")?),
    }
}

fn json_string(writer: &mut impl Write, value: &str) -> io::Result<()> {
    Ok(write!(writer, "{}", JsonString(value))?)
}

fn path_encoding() -> &'static str {
    if cfg!(unix) {
        "unix-bytes"
    } else if cfg!(windows) {
        "windows-utf16le"
    } else {
        "rust-os-string"
    }
}
fn path_hex(writer: &mut impl Write, path: &OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        for byte in path.as_bytes() {
            write!(writer, "{byte:02x}")?;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.encode_wide() {
            for byte in unit.to_le_bytes() {
                write!(writer, "{byte:02x}")?;
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    for byte in path.as_encoded_bytes() {
        write!(writer, "{byte:02x}")?;
    }
    Ok(())
}
