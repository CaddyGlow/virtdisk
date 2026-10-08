//! Worker-only import interception. No production source or DLL code is patched.
use super::{Ack, Identity, find_flush_iat_with};
use std::{
    io,
    os::windows::io::AsRawHandle,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::*,
    System::{
        Console::*, Diagnostics::Debug::ReadProcessMemory, LibraryLoader::*, Memory::*,
        Threading::*,
    },
};
type Flush = unsafe extern "system" fn(HANDLE) -> i32;
static ORIGINAL: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static INPUT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static OUTPUT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
static VOLUME: AtomicU32 = AtomicU32::new(0);
static FILE: AtomicU64 = AtomicU64::new(0);
static THREAD: AtomicU32 = AtomicU32::new(0);
static CUT: AtomicU32 = AtomicU32::new(0);
static FAILED: AtomicBool = AtomicBool::new(false);
pub fn identity(path: &std::path::Path) -> io::Result<Identity> {
    let file = std::fs::File::open(path)?;
    identity_handle(file.as_raw_handle())
}
fn identity_handle(handle: HANDLE) -> io::Result<Identity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity {
        volume: info.dwVolumeSerialNumber,
        file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}
unsafe extern "system" fn intercepted(handle: HANDLE) -> i32 {
    // These atomics and Win32 calls do not allocate; the acknowledgement is stack-only.
    let incoming = unsafe { GetLastError() };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let own = unsafe { GetFileInformationByHandle(handle, &mut info) } != 0
        && info.dwVolumeSerialNumber == VOLUME.load(Ordering::Relaxed)
        && ((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
            == FILE.load(Ordering::Relaxed);
    unsafe { SetLastError(incoming) };
    let original: Flush = unsafe { std::mem::transmute(ORIGINAL.load(Ordering::Acquire)) };
    let result = unsafe { original(handle) };
    let error = unsafe { GetLastError() };
    if result != 0 && own {
        if unsafe { GetCurrentThreadId() } != THREAD.load(Ordering::Relaxed) {
            FAILED.store(true, Ordering::Relaxed);
        } else {
            let cut = CUT.fetch_add(1, Ordering::Relaxed) + 1;
            if cut > 128 {
                FAILED.store(true, Ordering::Relaxed);
            } else {
                let ack = Ack::flushed(
                    cut,
                    Identity {
                        volume: VOLUME.load(Ordering::Relaxed),
                        file: FILE.load(Ordering::Relaxed),
                    },
                )
                .encode();
                let mut written = 0;
                if unsafe {
                    WriteFile(
                        OUTPUT.load(Ordering::Relaxed),
                        ack.as_ptr(),
                        32,
                        &mut written,
                        std::ptr::null_mut(),
                    )
                } == 0
                    || written != 32
                {
                    FAILED.store(true, Ordering::Relaxed);
                } else {
                    let mut go = 0u8;
                    let mut read = 0;
                    if unsafe {
                        ReadFile(
                            INPUT.load(Ordering::Relaxed),
                            &mut go,
                            1,
                            &mut read,
                            std::ptr::null_mut(),
                        )
                    } == 0
                        || read != 1
                        || go != 1
                    {
                        FAILED.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
    }
    unsafe { SetLastError(error) };
    result
}
pub struct Hook {
    slot: *mut usize,
    original: usize,
}
fn replace(slot: *mut usize, value: usize) -> io::Result<()> {
    let mut protect = 0;
    if unsafe { VirtualProtect(slot.cast(), 8, PAGE_READWRITE, &mut protect) } == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { std::ptr::write_volatile(slot, value) };
    let mut ignored = 0;
    if unsafe { VirtualProtect(slot.cast(), 8, protect, &mut ignored) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
impl Hook {
    pub fn install(id: Identity) -> io::Result<Self> {
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        let base = module as usize;
        let rva = find_flush_iat_with(|at, n| {
            let address = base
                .checked_add(at)
                .ok_or_else(|| io::Error::other("module address overflow"))?;
            let mut b = vec![0; n];
            let mut got = 0;
            if unsafe {
                ReadProcessMemory(
                    GetCurrentProcess(),
                    address as *const _,
                    b.as_mut_ptr().cast(),
                    n,
                    &mut got,
                )
            } == 0
                || got != n
            {
                return Err(io::Error::last_os_error());
            }
            Ok(b)
        })?;
        let slot = (base + rva) as *mut usize;
        if !(slot as usize).is_multiple_of(8) {
            return Err(io::Error::other("unaligned import slot"));
        }
        let original = unsafe { std::ptr::read_volatile(slot) };
        let kernel: Vec<_> = "kernel32.dll".encode_utf16().chain(Some(0)).collect();
        let kernel = unsafe { GetModuleHandleW(kernel.as_ptr()) };
        let api = unsafe { GetProcAddress(kernel, c"FlushFileBuffers".as_ptr().cast()) }
            .ok_or_else(|| io::Error::other("no native FlushFileBuffers export"))?
            as usize;
        if original != api {
            return Err(io::Error::other(
                "import does not reference verified real FlushFileBuffers",
            ));
        }
        let input = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if unsafe { GetFileType(input) } != FILE_TYPE_PIPE
            || unsafe { GetFileType(output) } != FILE_TYPE_PIPE
        {
            return Err(io::Error::other("worker requires established IPC pipes"));
        }
        INPUT.store(input, Ordering::Relaxed);
        OUTPUT.store(output, Ordering::Relaxed);
        VOLUME.store(id.volume, Ordering::Relaxed);
        FILE.store(id.file, Ordering::Relaxed);
        THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
        CUT.store(0, Ordering::Relaxed);
        FAILED.store(false, Ordering::Relaxed);
        ORIGINAL.store(original as *mut (), Ordering::Release);
        replace(slot, intercepted as *const () as usize)?;
        Ok(Self { slot, original })
    }
    pub fn finish(mut self) -> io::Result<u32> {
        replace(self.slot, self.original)?;
        self.slot = std::ptr::null_mut();
        if FAILED.load(Ordering::Relaxed) {
            return Err(io::Error::other("worker flush observation/IPC failed"));
        }
        let cuts = CUT.load(Ordering::Relaxed);
        if cuts == 0 {
            return Err(io::Error::other(
                "production sync bypassed intercepted import",
            ));
        }
        Ok(cuts)
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        if !self.slot.is_null() {
            let _ = replace(self.slot, self.original);
        }
    }
}
pub fn terminate(child: &std::process::Child) -> io::Result<()> {
    if unsafe { TerminateProcess(child.as_raw_handle(), 0x56444355) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
