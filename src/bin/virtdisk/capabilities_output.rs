//! Current-handle capability reports without reopening an inspected image.
use std::io::{self, Write};
use virtdisk::{Capability, ImageInspection, ImageOperation, ImageProfile, ValidationLevel};

pub(super) fn write_json(output: &mut impl Write, inspection: ImageInspection) -> io::Result<()> {
    let access = if inspection.capabilities.get(ImageOperation::Write) == Capability::Supported {
        "read-write"
    } else {
        "read-only"
    };
    write!(
        output,
        "{{\"type\":\"capabilities\",\"scope\":\"opened-handle\",\"access\":\"{access}\",\"profile\":"
    )?;
    match inspection.profile {
        ImageProfile::Raw => write!(output, "{{\"format\":\"raw\"}}")?,
        ImageProfile::Qcow2 { version } => {
            write!(output, "{{\"format\":\"qcow2\",\"version\":{version}}}")?
        }
        ImageProfile::Vdi { dynamic } => write!(
            output,
            "{{\"format\":\"vdi\",\"version\":\"1.1\",\"dynamic\":{dynamic}}}"
        )?,
        ImageProfile::Vmdk { descriptor } => write!(
            output,
            "{{\"format\":\"vmdk\",\"descriptor\":{descriptor}}}"
        )?,
        ImageProfile::Vhdx => write!(output, "{{\"format\":\"vhdx\",\"version\":1}}")?,
    }
    let validation = match inspection.validation {
        ValidationLevel::RegularFile => "regular-file",
        ValidationLevel::MappingBounds => "mapping-bounds",
        ValidationLevel::ActiveOwnership => "active-ownership",
    };
    write!(
        output,
        ",\"validation\":\"{validation}\",\"virtual_size\":{},\"has_parent\":{},\"native_snapshots\":",
        inspection.geometry.virtual_size, inspection.has_parent
    )?;
    match inspection.native_snapshots {
        Some(count) => write!(output, "{count}")?,
        None => write!(output, "null")?,
    }
    write!(output, ",\"operations\":[")?;
    for (index, (operation, capability)) in inspection.capabilities.iter().enumerate() {
        if index != 0 {
            write!(output, ",")?;
        }
        write!(
            output,
            "{{\"operation\":\"{}\",\"supported\":{},\"reason\":",
            operation.as_str(),
            capability == Capability::Supported
        )?;
        match capability {
            Capability::Supported => write!(output, "null")?,
            Capability::Unsupported(reason) => write!(output, "\"{}\"", reason.as_str())?,
        }
        write!(output, "}}")?;
    }
    writeln!(output, "]}}")
}
