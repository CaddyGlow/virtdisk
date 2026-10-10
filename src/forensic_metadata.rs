//! Native container declarations for forensic interpretation.
use crate::io;
use std::collections::BTreeMap;

/// Format-specific declarations from an opened immutable image.
/// Identifiers are recorded claims, not cryptographic identities or timestamps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerMetadata {
    /// Raw has no native container metadata.
    Raw,
    /// Recorded QCOW2 snapshot declarations, including unauthenticated timestamps.
    Qcow2 {
        /// Native snapshots in directory order; listing does not audit mapping ownership.
        snapshots: Vec<crate::Qcow2Snapshot>,
    },
    /// VirtualBox header UUIDs, in their original on-disk byte order.
    Vdi(VdiMetadata),
    /// VMware descriptor, or absence for a binary extent without a descriptor.
    Vmdk(Option<VmdkMetadata>),
    /// Microsoft VHDX headers and metadata.
    Vhdx(VhdxMetadata),
}

/// Extract native declarations without modifying the opened image.
pub trait InspectMetadata {
    /// Read format-specific metadata under the retained parser budget.
    /// Sources must remain immutable. A recovered reader reports its effective
    /// view, which may differ from the acquired file's original bytes.
    fn metadata(&self) -> io::Result<ContainerMetadata>;
}

/// Native VDI UUID fields. All arrays preserve the 16 bytes stored in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VdiMetadata {
    /// Image creation identity; does not encode a trusted creation date.
    pub creation_uuid: [u8; 16],
    /// Modification epoch identity; does not encode a modification date.
    pub modification_uuid: [u8; 16],
    /// Declared direct parent's creation identity; zero for standalone images.
    pub parent_uuid: [u8; 16],
    /// Declared direct parent's modification epoch; zero for standalone images.
    pub parent_modification_uuid: [u8; 16],
}

/// Native VMDK descriptor fields, preserving property values as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmdkMetadata {
    /// Content ID; a 32-bit epoch marker, not a content hash.
    pub cid: u32,
    /// Declared parent content ID; `ffffffff` denotes no parent.
    pub parent_cid: u32,
    /// All accepted properties, including vendor `ddb.*` values.
    /// Values retain quotes where present. Repeated vendor keys are preserved.
    pub properties: Vec<(String, String)>,
    /// Descriptor extent declarations in their original order.
    pub extents: Vec<VmdkExtentMetadata>,
}

/// One native VMDK extent declaration; names do not authorize filesystem access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmdkExtentMetadata {
    /// Declared access mode.
    pub access: String,
    /// Capacity in 512-byte sectors.
    pub sectors: u64,
    /// Declared extent type.
    pub kind: String,
    /// Unquoted declared filename.
    pub name: String,
    /// Starting 512-byte sector for a FLAT extent; absent for SPARSE extents.
    pub offset_sectors: Option<u64>,
}

/// Fields from a checksum-valid VHDX header; GUID arrays use native disk byte order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VhdxHeaderMetadata {
    /// Header byte offset: 65536 or 131072.
    pub offset: u64,
    /// Header generation counter; does not represent wall-clock time.
    pub sequence_number: u64,
    /// File-write epoch GUID.
    pub file_write_guid: [u8; 16],
    /// Data-write epoch GUID used for parent linkage.
    pub data_write_guid: [u8; 16],
    /// Native log GUID in this reader's effective view.
    pub log_guid: [u8; 16],
}

/// VHDX headers and native parent locator key/value declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VhdxMetadata {
    /// Both header slots; `None` means that slot failed signature/checksum validation.
    pub headers: [Option<VhdxHeaderMetadata>; 2],
    /// Offset of the active header selected by the parser.
    pub active_header_offset: u64,
    /// Virtual disk identifier from the required metadata item, in native disk byte order.
    pub virtual_disk_id: [u8; 16],
    /// All declared locator keys and decoded UTF-16 values, including unknown keys.
    /// Empty for standalone images. These are hints, not authorized host paths.
    pub parent_locator: BTreeMap<String, String>,
}

impl InspectMetadata for crate::RawDisk {
    fn metadata(&self) -> io::Result<ContainerMetadata> {
        Ok(ContainerMetadata::Raw)
    }
}
impl InspectMetadata for crate::Qcow2 {
    fn metadata(&self) -> io::Result<ContainerMetadata> {
        Ok(ContainerMetadata::Qcow2 {
            snapshots: self.list_snapshots()?,
        })
    }
}
