//! Native, disposable loader test. A 64 KiB stack exercises the same Windows
//! thread-attach and version-forwarder entry points as game SDK worker threads.
#[cfg(windows)]
fn main() {
    use core::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HMODULE};
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_sys::Win32::System::Threading::{
        CreateThread, GetExitCodeThread, WaitForSingleObject, STACK_SIZE_PARAM_IS_A_RESERVATION,
    };
    #[link(name = "kernel32")]
    extern "system" {
        fn SetErrorMode(mode: u32) -> u32;
    }
    type Entry = unsafe extern "system" fn(HMODULE, u32, *mut c_void) -> BOOL;
    type Size = unsafe extern "system" fn(*const u16, *mut u32) -> u32;
    struct Args {
        module: HMODULE,
        entry: Entry,
        size: Size,
        version: bool,
        file: Vec<u16>,
    }
    unsafe extern "system" fn worker(param: *mut c_void) -> u32 {
        let p = &*param.cast::<Args>();
        if (p.entry)(p.module, 2, core::ptr::null_mut()) == 0 {
            return 2;
        }
        if p.version {
            let mut handle = 0;
            if (p.size)(p.file.as_ptr(), &mut handle) == 0 {
                return 3;
            }
        }
        0
    }
    unsafe {
        // Native faults should return a failing exit code, not block a test on
        // a Windows Error Reporting dialog.
        SetErrorMode(0x0001 | 0x0002);
        let mut args = std::env::args_os().skip(1);
        let dll = args.next().expect("DLL path required");
        let version = args.next().is_some_and(|a| a == "version");
        let dll: Vec<u16> = dll.encode_wide().chain(Some(0)).collect();
        let module = LoadLibraryExW(dll.as_ptr(), core::ptr::null_mut(), 0);
        assert!(!module.is_null(), "cannot load exporter DLL");
        let entry: Entry = core::mem::transmute(
            GetProcAddress(module, c"DllMain".as_ptr().cast()).expect("DllMain export"),
        );
        let size: Size = core::mem::transmute(
            GetProcAddress(module, c"GetFileVersionInfoSizeW".as_ptr().cast())
                .expect("version export"),
        );
        let mut path = vec![0u16; 32768];
        let n = GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) as usize;
        assert!(n > 0 && n < path.len());
        path.truncate(n);
        path.extend("\\kernel32.dll\0".encode_utf16());
        let mut p = Args {
            module,
            entry,
            size,
            version,
            file: path,
        };
        let thread = CreateThread(
            core::ptr::null(),
            64 * 1024,
            Some(worker),
            (&mut p as *mut Args).cast(),
            STACK_SIZE_PARAM_IS_A_RESERVATION,
            core::ptr::null_mut(),
        );
        assert!(!thread.is_null(), "cannot create small-stack worker");
        assert_eq!(
            WaitForSingleObject(thread, 10_000),
            0,
            "small-stack worker did not finish"
        );
        let mut result = 1;
        assert!(GetExitCodeThread(thread, &mut result) != 0);
        CloseHandle(thread);
        assert_eq!(result, 0, "small-stack entry/forwarder failed");
        println!(
            "Native small-stack smoke passed: {}",
            if version {
                "version API"
            } else {
                "thread attach"
            }
        );
    }
}
#[cfg(not(windows))]
fn main() {
    panic!("Windows native loader test");
}
