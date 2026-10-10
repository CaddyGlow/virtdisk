//! JSON rendering of native declarations with explicit byte encodings.
use super::json_output::JsonString;
use virtdisk::ContainerMetadata;
use virtdisk::io::{self, Write};

fn bytes(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    write!(output, "\"")?;
    for byte in bytes {
        write!(output, "{byte:02x}")?;
    }
    Ok(write!(output, "\"")?)
}

pub(super) fn write_json(output: &mut impl Write, metadata: &ContainerMetadata) -> io::Result<()> {
    write!(
        output,
        "{{\"scope\":\"effective-container-declarations\",\"format\":"
    )?;
    match metadata {
        ContainerMetadata::Raw => write!(output, "\"raw\",\"native_metadata\":null")?,
        ContainerMetadata::Qcow2 { snapshots } => {
            write!(output, "\"qcow2\",\"snapshots\":[")?;
            for (index, snapshot) in snapshots.iter().enumerate() {
                if index != 0 {
                    write!(output, ",")?;
                }
                write!(output, "{{\"id_hex\":")?;
                bytes(output, &snapshot.id)?;
                write!(output, ",\"name_hex\":")?;
                bytes(output, &snapshot.name)?;
                write!(
                    output,
                    ",\"virtual_size\":{},\"vm_state_size\":{},\"date_seconds\":{},\"date_nanoseconds\":{},\"vm_clock_nanoseconds\":\"{}\",\"l1_table_offset\":{},\"l1_entries\":{}}}",
                    snapshot.virtual_size,
                    snapshot.vm_state_size,
                    snapshot.date_seconds,
                    snapshot.date_nanoseconds,
                    snapshot.vm_clock_nanoseconds,
                    snapshot.l1_table_offset,
                    snapshot.l1_entries
                )?;
            }
            write!(output, "]")?;
        }
        ContainerMetadata::Vdi(value) => {
            write!(output, "\"vdi\"")?;
            for (name, value) in [
                ("creation_uuid_hex", &value.creation_uuid),
                ("modification_uuid_hex", &value.modification_uuid),
                ("parent_uuid_hex", &value.parent_uuid),
                (
                    "parent_modification_uuid_hex",
                    &value.parent_modification_uuid,
                ),
            ] {
                write!(output, ",\"{name}\":")?;
                bytes(output, value)?;
            }
        }
        ContainerMetadata::Vmdk(value) => {
            write!(output, "\"vmdk\",\"descriptor\":")?;
            if let Some(value) = value {
                write!(
                    output,
                    "{{\"cid\":\"{:08x}\",\"parent_cid\":\"{:08x}\",\"properties\":[",
                    value.cid, value.parent_cid
                )?;
                for (index, (key, value)) in value.properties.iter().enumerate() {
                    if index != 0 {
                        write!(output, ",")?;
                    }
                    write!(
                        output,
                        "{{\"key\":{},\"value\":{}}}",
                        JsonString(key),
                        JsonString(value)
                    )?;
                }
                write!(output, "],\"extents\":[")?;
                for (index, extent) in value.extents.iter().enumerate() {
                    if index != 0 {
                        write!(output, ",")?;
                    }
                    write!(
                        output,
                        "{{\"access\":{},\"sectors\":{},\"kind\":{},\"name\":{},\"offset_sectors\":",
                        JsonString(&extent.access),
                        extent.sectors,
                        JsonString(&extent.kind),
                        JsonString(&extent.name)
                    )?;
                    match extent.offset_sectors {
                        Some(offset) => write!(output, "{offset}")?,
                        None => write!(output, "null")?,
                    }
                    write!(output, "}}")?;
                }
                write!(output, "]}}")?;
            } else {
                write!(output, "null")?;
            }
        }
        ContainerMetadata::Vhdx(value) => {
            write!(
                output,
                "\"vhdx\",\"active_header_offset\":{},\"virtual_disk_id_hex\":",
                value.active_header_offset
            )?;
            bytes(output, &value.virtual_disk_id)?;
            write!(output, ",\"headers\":[")?;
            for (index, header) in value.headers.iter().enumerate() {
                if index != 0 {
                    write!(output, ",")?;
                }
                if let Some(header) = header {
                    // Render u64 counters as decimal strings to preserve precision in JSON consumers.
                    write!(
                        output,
                        "{{\"offset\":{},\"sequence_number\":\"{}\"",
                        header.offset, header.sequence_number
                    )?;
                    for (name, value) in [
                        ("file_write_guid_hex", &header.file_write_guid),
                        ("data_write_guid_hex", &header.data_write_guid),
                        ("log_guid_hex", &header.log_guid),
                    ] {
                        write!(output, ",\"{name}\":")?;
                        bytes(output, value)?;
                    }
                    write!(output, "}}")?;
                } else {
                    write!(output, "null")?;
                }
            }
            write!(output, "],\"parent_locator\":{{")?;
            for (index, (key, value)) in value.parent_locator.iter().enumerate() {
                if index != 0 {
                    write!(output, ",")?;
                }
                write!(output, "{}:{}", JsonString(key), JsonString(value))?;
            }
            write!(output, "}}")?;
        }
    }
    Ok(writeln!(output, "}}")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_runtime_and_binary_names_are_losslessly_rendered() {
        let metadata = ContainerMetadata::Qcow2 {
            snapshots: vec![virtdisk::Qcow2Snapshot {
                id: vec![0, 255],
                name: vec![128],
                virtual_size: 512,
                vm_state_size: 0,
                date_seconds: 42,
                date_nanoseconds: 123456789,
                vm_clock_nanoseconds: u64::MAX,
                l1_table_offset: 65536,
                l1_entries: 1,
            }],
        };
        let mut bytes = Vec::new();
        write_json(&mut bytes, &metadata).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("\"id_hex\":\"00ff\",\"name_hex\":\"80\""));
        assert!(output.contains("\"date_seconds\":42,\"date_nanoseconds\":123456789"));
        assert!(output.contains("\"vm_clock_nanoseconds\":\"18446744073709551615\""));
    }
}
