use crate::Manifest;
use sha2::{Digest, Sha256};
use std::{io, os::windows::ffi::OsStrExt, path::Path};
use windows_sys::Win32::{
    Foundation::*,
    Storage::{FileSystem::*, Vhd::*},
};
struct Disk {
    handle: HANDLE,
    attached: bool,
}
impl Drop for Disk {
    fn drop(&mut self) {
        unsafe {
            if self.attached {
                DetachVirtualDisk(self.handle, 0, 0);
            }
            CloseHandle(self.handle);
        }
    }
}
struct Device(HANDLE);
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn status(code: u32, operation: &str) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation}: Windows code {code}: {}; run elevated with SeManageVolumePrivilege enabled for attach",
            io::Error::from_raw_os_error(code as i32)
        )))
    }
}
unsafe fn info(handle: HANDLE, version: i32) -> io::Result<GET_VIRTUAL_DISK_INFO> {
    let mut value = GET_VIRTUAL_DISK_INFO {
        Version: version,
        ..Default::default()
    };
    let mut length = std::mem::size_of_val(&value) as u32;
    status(
        unsafe { GetVirtualDiskInformation(handle, &mut length, &mut value, std::ptr::null_mut()) },
        "GetVirtualDiskInformation",
    )?;
    Ok(value)
}
pub fn readout(path: &Path, m: &Manifest) -> io::Result<serde_json::Value> {
    enable_manage_volume()?;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let storage = VIRTUAL_STORAGE_TYPE {
        DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    };
    let policy = crate::native_open_policy(m.mode);
    let params = OPEN_VIRTUAL_DISK_PARAMETERS {
        Version: OPEN_VIRTUAL_DISK_VERSION_1,
        Anonymous: OPEN_VIRTUAL_DISK_PARAMETERS_0 {
            Version1: OPEN_VIRTUAL_DISK_PARAMETERS_0_0 {
                RWDepth: policy.rw_depth,
            },
        },
    };
    let mut handle = std::ptr::null_mut();
    // Only a validated harness-owned image path can reach this call.
    unsafe {
        status(
            OpenVirtualDisk(
                &storage,
                name.as_ptr(),
                policy.access_mask,
                0,
                &params,
                &mut handle,
            ),
            "OpenVirtualDisk",
        )?;
    }
    let mut disk = Disk {
        handle,
        attached: false,
    };
    let size = unsafe { info(handle, GET_VIRTUAL_DISK_INFO_SIZE)?.Anonymous.Size };
    let physical = unsafe {
        info(handle, GET_VIRTUAL_DISK_INFO_VHD_PHYSICAL_SECTOR_SIZE)?
            .Anonymous
            .VhdPhysicalSectorSize
    };
    if size.VirtualSize != m.capacity
        || size.SectorSize != m.logical_sector
        || physical != m.physical_sector
    {
        return Err(io::Error::other("native geometry disagrees with manifest"));
    }
    let attach = ATTACH_VIRTUAL_DISK_PARAMETERS {
        Version: ATTACH_VIRTUAL_DISK_VERSION_1,
        ..Default::default()
    };
    unsafe {
        status(
            AttachVirtualDisk(
                handle,
                std::ptr::null_mut(),
                policy.attach_flags,
                0,
                &attach,
                std::ptr::null(),
            ),
            "AttachVirtualDisk",
        )?;
    }
    disk.attached = true;
    let mut physical_name = [0u16; 1024];
    let mut length = 2048;
    unsafe {
        status(
            GetVirtualDiskPhysicalPath(handle, &mut length, physical_name.as_mut_ptr()),
            "GetVirtualDiskPhysicalPath",
        )?;
    }
    // No user-supplied device path: this comes exclusively from our attached handle.
    let device = unsafe {
        CreateFileW(
            physical_name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if device == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let device = Device(device);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut left = m.capacity;
    while left != 0 {
        let count = left.min(buffer.len() as u64) as u32;
        let mut read = 0;
        let result = unsafe {
            ReadFile(
                device.0,
                buffer.as_mut_ptr(),
                count,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        if read != count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short native disk read",
            ));
        }
        hash.update(&buffer[..read as usize]);
        left -= u64::from(read);
    }
    drop(device);
    let hash = format!("{:x}", hash.finalize());
    unsafe {
        status(DetachVirtualDisk(handle, 0, 0), "DetachVirtualDisk")?;
    }
    disk.attached = false;
    if hash != m.logical_sha256 {
        return Err(io::Error::other("native logical SHA256 mismatch"));
    }
    Ok(
        serde_json::json!({"logical_sha256":hash,"capacity":size.VirtualSize,"logical_sector":size.SectorSize,"physical_sector":physical,"block_size":size.BlockSize,"open":0,"attach":0,"detach":0}),
    )
}

fn enable_manage_volume() -> io::Result<()> {
    use windows_sys::Win32::{Security::*, System::Threading::*};
    let mut token = std::ptr::null_mut();
    unsafe {
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let token = Device(token);
    let name: Vec<u16> = "SeManageVolumePrivilege"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut luid = LUID::default();
    unsafe {
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let state = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        SetLastError(0);
        if AdjustTokenPrivileges(
            token.0,
            0,
            &state,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        status(GetLastError(), "enable SeManageVolumePrivilege")?;
    }
    Ok(())
}

/// Produce only a new disposable native child of the validated copied parent.
pub fn produce_child(parent: &Path, child: &Path, m: &Manifest) -> io::Result<serde_json::Value> {
    let plan = crate::producer_plan(m)?;
    enable_manage_volume()?;
    if child.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "child already exists",
        ));
    }
    let parent_name: Vec<_> = parent.as_os_str().encode_wide().chain(Some(0)).collect();
    let child_name: Vec<_> = child.as_os_str().encode_wide().chain(Some(0)).collect();
    let storage = VIRTUAL_STORAGE_TYPE {
        DeviceId: VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    };
    let create = CREATE_VIRTUAL_DISK_PARAMETERS {
        Version: CREATE_VIRTUAL_DISK_VERSION_2,
        Anonymous: CREATE_VIRTUAL_DISK_PARAMETERS_0 {
            Version2: CREATE_VIRTUAL_DISK_PARAMETERS_0_1 {
                ParentPath: parent_name.as_ptr(),
                ParentVirtualStorageType: storage,
                ..Default::default()
            },
        },
    };
    let mut handle = std::ptr::null_mut();
    unsafe {
        status(
            CreateVirtualDisk(
                &storage,
                child_name.as_ptr(),
                VIRTUAL_DISK_ACCESS_NONE,
                std::ptr::null_mut(),
                CREATE_VIRTUAL_DISK_FLAG_NONE,
                0,
                &create,
                std::ptr::null_mut(),
                &mut handle,
            ),
            "CreateVirtualDisk child",
        )?;
        CloseHandle(handle);
    }
    let open = OPEN_VIRTUAL_DISK_PARAMETERS {
        Version: OPEN_VIRTUAL_DISK_VERSION_1,
        Anonymous: OPEN_VIRTUAL_DISK_PARAMETERS_0 {
            Version1: OPEN_VIRTUAL_DISK_PARAMETERS_0_0 { RWDepth: 1 },
        },
    };
    unsafe {
        status(
            OpenVirtualDisk(
                &storage,
                child_name.as_ptr(),
                VIRTUAL_DISK_ACCESS_ATTACH_RW
                    | VIRTUAL_DISK_ACCESS_GET_INFO
                    | VIRTUAL_DISK_ACCESS_DETACH,
                0,
                &open,
                &mut handle,
            ),
            "OpenVirtualDisk produced child",
        )?;
    }
    let mut disk = Disk {
        handle,
        attached: false,
    };
    let geometry = unsafe { info(handle, GET_VIRTUAL_DISK_INFO_SIZE)?.Anonymous.Size };
    let physical = unsafe {
        info(handle, GET_VIRTUAL_DISK_INFO_VHD_PHYSICAL_SECTOR_SIZE)?
            .Anonymous
            .VhdPhysicalSectorSize
    };
    if geometry.VirtualSize != m.capacity
        || geometry.SectorSize != m.logical_sector
        || physical != m.physical_sector
    {
        return Err(io::Error::other("produced child native geometry mismatch"));
    }
    let attach = ATTACH_VIRTUAL_DISK_PARAMETERS {
        Version: ATTACH_VIRTUAL_DISK_VERSION_1,
        ..Default::default()
    };
    unsafe {
        status(
            AttachVirtualDisk(
                handle,
                std::ptr::null_mut(),
                ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER,
                0,
                &attach,
                std::ptr::null(),
            ),
            "AttachVirtualDisk produced child",
        )?;
    }
    disk.attached = true;
    let mut name = [0u16; 1024];
    let mut length = 2048;
    unsafe {
        status(
            GetVirtualDiskPhysicalPath(handle, &mut length, name.as_mut_ptr()),
            "GetVirtualDiskPhysicalPath produced child",
        )?;
    }
    // Only the just-created child handle supplies this path; no user device path.
    let device = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if device == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let device = Device(device);
    for (offset, bytes) in &plan {
        let mut written = 0;
        unsafe {
            if SetFilePointerEx(device.0, *offset as i64, std::ptr::null_mut(), FILE_BEGIN) == 0
                || WriteFile(
                    device.0,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        if written as usize != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short produced sector write",
            ));
        }
    }
    unsafe {
        if FlushFileBuffers(device.0) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    drop(device);
    unsafe {
        status(
            DetachVirtualDisk(handle, 0, 0),
            "DetachVirtualDisk produced child",
        )?;
    }
    disk.attached = false;
    drop(disk);
    let mut expected = vec![7u8; m.capacity as usize];
    for (offset, bytes) in &plan {
        expected[*offset as usize..*offset as usize + bytes.len()].copy_from_slice(bytes);
    }
    let mut check = Manifest {
        version: 1,
        image: "child.vhdx".into(),
        parents: vec![],
        files: vec![],
        capacity: m.capacity,
        logical_sector: m.logical_sector,
        physical_sector: m.physical_sector,
        logical_sha256: format!("{:x}", Sha256::digest(expected)),
        mode: crate::Mode::Clean,
    };
    let native = readout(child, &check)?;
    check.parents.push("base.vhdx".into());
    Ok(
        serde_json::json!({"create":0,"writes":plan.iter().map(|(o,b)|serde_json::json!({"offset":o,"length":b.len(),"byte":b[0]})).collect::<Vec<_>>(),"native":native,"logical_sha256":check.logical_sha256,"capacity":check.capacity,"logical_sector":check.logical_sector}),
    )
}
