//! Resolve the real version APIs from the system directory at runtime.
//! A full OS-derived path prevents loading this proxy again and supports non-default Windows locations.
use core::ffi::c_void;
use std::sync::OnceLock;
use windows_sys::core::{PCSTR, PCWSTR, PSTR, PWSTR};
use windows_sys::Win32::Foundation::{SetLastError, BOOL, ERROR_PROC_NOT_FOUND};
use windows_sys::Win32::Storage::FileSystem::{
    GET_FILE_VERSION_INFO_FLAGS, VER_FIND_FILE_FLAGS, VER_FIND_FILE_STATUS, VER_INSTALL_FILE_FLAGS,
    VER_INSTALL_FILE_STATUS,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

unsafe fn system_version() -> usize {
    static MODULE: OnceLock<usize> = OnceLock::new();
    *MODULE.get_or_init(|| {
        // Version queries also run on small-stack native SDK threads. Keep the
        // long OS path buffer on the heap rather than reserving a 64 KiB frame.
        let mut path = vec![0u16; 32768];
        let count = unsafe { GetSystemDirectoryW(path.as_mut_ptr(), path.len() as u32) } as usize;
        let suffix: Vec<u16> = "\\version.dll\0".encode_utf16().collect();
        if count == 0 || count + suffix.len() > path.len() {
            return 0;
        }
        path[count..count + suffix.len()].copy_from_slice(&suffix);
        unsafe {
            LoadLibraryExW(
                path.as_ptr(),
                core::ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            ) as usize
        }
    })
}

macro_rules! version_api {
    ($name:ident ($($arg:ident : $ty:ty),*) -> $result:ty) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name($($arg: $ty),*) -> $result {
            type Api = unsafe extern "system" fn($($ty),*) -> $result;
            static API: OnceLock<Option<Api>> = OnceLock::new();
            let api = API.get_or_init(|| {
                let module = unsafe { system_version() };
                if module == 0 { return None; }
                unsafe { GetProcAddress(module as _, concat!(stringify!($name), "\0").as_ptr()) }
                    .map(|address| unsafe { core::mem::transmute::<unsafe extern "system" fn() -> isize, Api>(address) })
            });
            match api {
                Some(call) => unsafe { call($($arg),*) },
                None => { unsafe { SetLastError(ERROR_PROC_NOT_FOUND) }; 0 }
            }
        }
    };
}
version_api!(GetFileVersionInfoA(lptstrfilename : PCSTR, dwhandle : u32, dwlen : u32, lpdata : *mut c_void) -> BOOL);
version_api!(GetFileVersionInfoExA(dwflags : GET_FILE_VERSION_INFO_FLAGS, lpwstrfilename : PCSTR, dwhandle : u32, dwlen : u32, lpdata : *mut c_void) -> BOOL);
version_api!(GetFileVersionInfoExW(dwflags : GET_FILE_VERSION_INFO_FLAGS, lpwstrfilename : PCWSTR, dwhandle : u32, dwlen : u32, lpdata : *mut c_void) -> BOOL);
version_api!(GetFileVersionInfoSizeA(lptstrfilename : PCSTR, lpdwhandle : *mut u32) -> u32);
version_api!(GetFileVersionInfoSizeExA(dwflags : GET_FILE_VERSION_INFO_FLAGS, lpwstrfilename : PCSTR, lpdwhandle : *mut u32) -> u32);
version_api!(GetFileVersionInfoSizeExW(dwflags : GET_FILE_VERSION_INFO_FLAGS, lpwstrfilename : PCWSTR, lpdwhandle : *mut u32) -> u32);
version_api!(GetFileVersionInfoSizeW(lptstrfilename : PCWSTR, lpdwhandle : *mut u32) -> u32);
version_api!(GetFileVersionInfoW(lptstrfilename : PCWSTR, dwhandle : u32, dwlen : u32, lpdata : *mut c_void) -> BOOL);
version_api!(VerFindFileA(uflags : VER_FIND_FILE_FLAGS, szfilename : PCSTR, szwindir : PCSTR, szappdir : PCSTR, szcurdir : PSTR, pucurdirlen : *mut u32, szdestdir : PSTR, pudestdirlen : *mut u32) -> VER_FIND_FILE_STATUS);
version_api!(VerFindFileW(uflags : VER_FIND_FILE_FLAGS, szfilename : PCWSTR, szwindir : PCWSTR, szappdir : PCWSTR, szcurdir : PWSTR, pucurdirlen : *mut u32, szdestdir : PWSTR, pudestdirlen : *mut u32) -> VER_FIND_FILE_STATUS);
version_api!(VerInstallFileA(uflags : VER_INSTALL_FILE_FLAGS, szsrcfilename : PCSTR, szdestfilename : PCSTR, szsrcdir : PCSTR, szdestdir : PCSTR, szcurdir : PCSTR, sztmpfile : PSTR, putmpfilelen : *mut u32) -> VER_INSTALL_FILE_STATUS);
version_api!(VerInstallFileW(uflags : VER_INSTALL_FILE_FLAGS, szsrcfilename : PCWSTR, szdestfilename : PCWSTR, szsrcdir : PCWSTR, szdestdir : PCWSTR, szcurdir : PCWSTR, sztmpfile : PWSTR, putmpfilelen : *mut u32) -> VER_INSTALL_FILE_STATUS);
version_api!(VerLanguageNameA(wlang : u32, szlang : PSTR, cchlang : u32) -> u32);
version_api!(VerLanguageNameW(wlang : u32, szlang : PWSTR, cchlang : u32) -> u32);
version_api!(VerQueryValueA(pblock : *const c_void, lpsubblock : PCSTR, lplpbuffer : *mut *mut c_void, pulen : *mut u32) -> BOOL);
version_api!(VerQueryValueW(pblock : *const c_void, lpsubblock : PCWSTR, lplpbuffer : *mut *mut c_void, pulen : *mut u32) -> BOOL);
