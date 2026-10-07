//! Inspection facts and capabilities of concrete opened handles.
use crate::{
    Qcow2, Qcow2Writer, RawDisk, RawWriter, ReadAt, Vdi, VdiWriter, Vhdx, VhdxWriter, Vmdk,
    VmdkWriter,
};
/// Geometry recorded by the container; absent values must not be guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskGeometry {
    /// Addressable logical bytes.
    pub virtual_size: u64,
    /// Guest logical sector size, when explicitly recorded.
    pub logical_sector_size: Option<u32>,
    /// Guest physical sector size, when explicitly recorded.
    pub physical_sector_size: Option<u32>,
    /// Container allocation unit; raw and multi-extent layouts may have none.
    pub allocation_block_size: Option<u64>,
}
/// Concrete profile successfully opened by the current handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProfile {
    /// Unstructured regular file.
    Raw,
    /// QCOW2 container version.
    Qcow2 {
        /// Header version.
        version: u32,
    },
    /// VirtualBox VDI v1.1.
    Vdi {
        /// Dynamic allocation layout rather than fixed.
        dynamic: bool,
    },
    /// Standalone hosted sparse binary extent or authorized external descriptor.
    Vmdk {
        /// Whether the handle uses an externally authorized descriptor.
        descriptor: bool,
    },
    /// Microsoft VHDX v1.
    Vhdx,
}
/// Scope of validation performed while opening this handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationLevel {
    /// A regular file was opened; no disk container structures exist.
    RegularFile,
    /// Header and individual mapping bounds; complete ownership was not audited.
    MappingBounds,
    /// Active mappings and allocation ownership were checked.
    ActiveOwnership,
}
/// An operation on the current handle, rather than a promise for the format family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOperation {
    /// Logical positional reading.
    Read,
    /// Logical positional writing.
    Write,
    /// Zero an existing logical range.
    WriteZeroes,
    /// Request host durability for completed writes.
    Flush,
    /// Change virtual capacity.
    Resize,
    /// Discard storage while retaining documented logical semantics.
    Discard,
    /// Request host allocation without changing logical bytes or capacity.
    Preallocate,
    /// Visit known allocation classifications.
    ExtentMap,
    /// Create or mutate native format snapshots.
    NativeSnapshot,
    /// Create a native disk snapshot, subject to directory/profile bounds.
    NativeSnapshotCreate,
    /// Delete a native disk snapshot while preserving remaining saved states.
    NativeSnapshotDelete,
    /// Restore active disk mappings from a retained native disk snapshot.
    NativeSnapshotRevert,
    /// Create a native differencing image.
    Derive,
    /// Change native backing relationships.
    Rebase,
    /// Merge native image chains.
    Merge,
    /// Reclaim physical container allocation.
    Compact,
}
/// Actionable reason an operation is unavailable on the current handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// Open a supported writer profile with exclusive access to mutate.
    ReadOnlyHandle,
    /// No implementation is available for this opened profile.
    NotImplemented,
    /// Allocation classification is not available from this handle.
    AllocationUnknown,
}
/// Availability of one operation on the opened handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Implemented for this opened profile, subject to documented bounds and I/O errors.
    Supported,
    /// Unavailable for the stated reason.
    Unsupported(UnsupportedReason),
}
/// Per-handle operation report; does not imply another profile can be opened writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCapabilities {
    writable: bool,
    resize: bool,
    extent_map: bool,
    discard: bool,
    preallocate: bool,
    native_snapshot: bool,
    snapshot_lifecycle: bool,
}
impl ImageCapabilities {
    /// Query a concrete operation on this handle.
    pub fn get(&self, operation: ImageOperation) -> Capability {
        use ImageOperation::*;
        match operation {
            Read => Capability::Supported,
            Write | WriteZeroes | Flush | Resize | Discard | Preallocate if !self.writable => {
                Capability::Unsupported(UnsupportedReason::ReadOnlyHandle)
            }
            Write | WriteZeroes | Flush => Capability::Supported,
            Resize if self.resize => Capability::Supported,
            Discard if self.discard => Capability::Supported,
            Preallocate if self.preallocate => Capability::Supported,
            NativeSnapshot if self.native_snapshot || self.snapshot_lifecycle => {
                Capability::Supported
            }
            NativeSnapshotCreate if self.native_snapshot => Capability::Supported,
            NativeSnapshotDelete | NativeSnapshotRevert if self.snapshot_lifecycle => {
                Capability::Supported
            }
            ExtentMap if self.extent_map => Capability::Supported,
            ExtentMap => Capability::Unsupported(UnsupportedReason::AllocationUnknown),
            Resize | Discard | Preallocate | NativeSnapshot | NativeSnapshotCreate
            | NativeSnapshotDelete | NativeSnapshotRevert | Derive | Rebase | Merge | Compact => {
                Capability::Unsupported(UnsupportedReason::NotImplemented)
            }
        }
    }
}
/// Validated facts and current-handle operations, without filesystem interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInspection {
    /// Concrete supported container profile.
    pub profile: ImageProfile,
    /// Container-declared disk geometry.
    pub geometry: DiskGeometry,
    /// Current opening mode and implemented operations.
    pub capabilities: ImageCapabilities,
    /// Scope already validated, not a fresh validation or authenticity claim.
    pub validation: ValidationLevel,
    /// Whether an authorized backing image is attached.
    pub has_parent: bool,
    /// Native snapshot count if declared by the format; absent otherwise.
    pub native_snapshots: Option<u32>,
}
/// Inspection of an already opened reader or writer, without opening another handle.
pub trait InspectImage {
    /// Return facts and capabilities retained by the current handle.
    fn inspection(&self) -> ImageInspection;
}
fn report(
    profile: ImageProfile,
    size: u64,
    logical: Option<u32>,
    physical: Option<u32>,
    block: Option<u64>,
    writer: bool,
    validation: ValidationLevel,
) -> ImageInspection {
    ImageInspection {
        profile,
        geometry: DiskGeometry {
            virtual_size: size,
            logical_sector_size: logical,
            physical_sector_size: physical,
            allocation_block_size: block,
        },
        capabilities: ImageCapabilities {
            writable: writer,
            resize: writer && profile == ImageProfile::Raw,
            extent_map: !writer && profile != ImageProfile::Raw,
            discard: writer && profile == ImageProfile::Raw && cfg!(target_os = "linux"),
            preallocate: writer && profile == ImageProfile::Raw && cfg!(target_os = "linux"),
            native_snapshot: false,
            snapshot_lifecycle: false,
        },
        validation,
        has_parent: false,
        native_snapshots: None,
    }
}
impl InspectImage for RawDisk {
    fn inspection(&self) -> ImageInspection {
        report(
            ImageProfile::Raw,
            self.len(),
            None,
            None,
            None,
            false,
            ValidationLevel::RegularFile,
        )
    }
}
impl InspectImage for RawWriter {
    fn inspection(&self) -> ImageInspection {
        report(
            ImageProfile::Raw,
            self.len(),
            None,
            None,
            None,
            true,
            ValidationLevel::RegularFile,
        )
    }
}
impl InspectImage for Qcow2 {
    fn inspection(&self) -> ImageInspection {
        let (version, block, parent, snapshots) = self.info_profile();
        let mut r = report(
            ImageProfile::Qcow2 { version },
            self.len(),
            None,
            None,
            Some(block),
            false,
            ValidationLevel::MappingBounds,
        );
        r.has_parent = parent;
        r.native_snapshots = Some(snapshots);
        r
    }
}
impl InspectImage for Qcow2Writer {
    fn inspection(&self) -> ImageInspection {
        let mut r = report(
            ImageProfile::Qcow2 { version: 3 },
            self.len(),
            None,
            None,
            Some(65536),
            true,
            ValidationLevel::ActiveOwnership,
        );
        r.native_snapshots = Some(self.native_snapshot_count());
        r.capabilities.native_snapshot = self.native_snapshot_creation_supported();
        r.capabilities.snapshot_lifecycle = self.native_snapshot_lifecycle_supported();
        r.capabilities.discard = cfg!(target_os = "linux");
        r.has_parent = self.has_parent();
        r.capabilities.resize =
            cfg!(target_os = "linux") && !self.has_parent() && self.native_snapshot_count() == 0;
        r
    }
}
impl InspectImage for Vdi {
    fn inspection(&self) -> ImageInspection {
        let (block, dynamic) = self.info_profile();
        let mut result = report(
            ImageProfile::Vdi { dynamic },
            self.len(),
            Some(512),
            None,
            Some(block),
            false,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result
    }
}
impl InspectImage for VdiWriter {
    fn inspection(&self) -> ImageInspection {
        let (block, dynamic) = self.info_profile();
        let mut result = report(
            ImageProfile::Vdi { dynamic },
            self.len(),
            Some(512),
            None,
            Some(block),
            true,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result.capabilities.discard = cfg!(target_os = "linux") && dynamic && block <= 1048576;
        result.capabilities.resize =
            cfg!(target_os = "linux") && dynamic && block <= 1048576 && !self.has_parent();
        result
    }
}
impl InspectImage for Vmdk {
    fn inspection(&self) -> ImageInspection {
        let (block, descriptor) = self.info_profile();
        let mut result = report(
            ImageProfile::Vmdk { descriptor },
            self.len(),
            None,
            None,
            block,
            false,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result
    }
}
impl InspectImage for VmdkWriter {
    fn inspection(&self) -> ImageInspection {
        let (block, descriptor) = self.info_profile();
        let mut result = report(
            ImageProfile::Vmdk { descriptor },
            self.len(),
            None,
            None,
            block,
            true,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result.capabilities.resize = self.native_resize_supported();
        result.capabilities.discard = self.native_discard_supported();
        result
    }
}
impl InspectImage for Vhdx {
    fn inspection(&self) -> ImageInspection {
        let (_, logical, physical, block) = self.geometry();
        let mut result = report(
            ImageProfile::Vhdx,
            self.len(),
            Some(logical),
            Some(physical),
            Some(block),
            false,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result
    }
}
impl InspectImage for VhdxWriter {
    fn inspection(&self) -> ImageInspection {
        let (_, logical, physical, block) = self.geometry();
        let mut result = report(
            ImageProfile::Vhdx,
            self.len(),
            Some(logical),
            Some(physical),
            Some(block),
            true,
            ValidationLevel::ActiveOwnership,
        );
        result.has_parent = self.has_parent();
        result.capabilities.discard = self.native_discard_supported();
        result.capabilities.resize = self.native_resize_supported();
        result
    }
}
