//! Minimal by-name bindings. No game RVAs, external interop or keys.
use anyhow::{ensure, Context, Result};
use core::ffi::{c_void, CStr};
use std::ffi::CString;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};

macro_rules! api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        pub struct Il2Cpp { $(pub $name: $ty,)* }
        impl Il2Cpp {
            pub fn load() -> Result<Self> {
                let module = unsafe { GetModuleHandleA(c"GameAssembly.dll".as_ptr().cast()) };
                ensure!(!module.is_null(), "GameAssembly.dll unavailable");
                unsafe { Ok(Self { $($name: {
                    let addr = GetProcAddress(module, concat!("il2cpp_", stringify!($name), "\0").as_ptr())
                        .context(concat!("missing il2cpp_", stringify!($name)))?;
                    core::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(addr)
                },)* }) }
            }
        }
    };
}
api! {
    domain_get: unsafe extern "C" fn() -> *mut c_void,
    get_corlib: unsafe extern "C" fn() -> *mut c_void,
    thread_attach: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    thread_detach: unsafe extern "C" fn(*mut c_void),
    domain_get_assemblies: unsafe extern "C" fn(*mut c_void, *mut usize) -> *mut *mut c_void,
    assembly_get_image: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    image_get_name: unsafe extern "C" fn(*mut c_void) -> *const i8,
    class_from_name: unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> *mut c_void,
    class_get_name: unsafe extern "C" fn(*mut c_void) -> *const i8,
    method_get_name: unsafe extern "C" fn(*mut c_void) -> *const i8,
    class_get_nested_types: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void,
    class_get_method_from_name: unsafe extern "C" fn(*mut c_void, *const i8, i32) -> *mut c_void,
    class_get_field_from_name: unsafe extern "C" fn(*mut c_void, *const i8) -> *mut c_void,
    field_get_offset: unsafe extern "C" fn(*mut c_void) -> usize,
    field_static_set_value: unsafe extern "C" fn(*mut c_void, *mut c_void),
    string_new: unsafe extern "C" fn(*const i8) -> *mut c_void,
    string_chars: unsafe extern "C" fn(*mut c_void) -> *const u16,
    string_length: unsafe extern "C" fn(*mut c_void) -> i32,
    object_get_class: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    object_new: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    object_unbox: unsafe extern "C" fn(*mut c_void) -> *mut c_void,
    array_new: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void,
    runtime_invoke: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut *mut c_void, *mut *mut c_void) -> *mut c_void,
    gchandle_new: unsafe extern "C" fn(*mut c_void, bool) -> usize,
    gchandle_free: unsafe extern "C" fn(usize),
    gchandle_get_target: unsafe extern "C" fn(usize) -> *mut c_void,
    gc_wbarrier_set_field: unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut c_void),
}

impl Il2Cpp {
    pub fn count(&self) -> usize {
        assembly_count(self.get_corlib, self.domain_get, self.domain_get_assemblies)
    }
    pub fn class(&self, assembly: &str, ns: &str, name: &str) -> Result<*mut c_void> {
        let mut n = 0;
        unsafe {
            let arr = (self.domain_get_assemblies)((self.domain_get)(), &mut n);
            ensure!(!arr.is_null() && n <= 100_000, "invalid assembly inventory");
            for i in 0..n {
                let img = (self.assembly_get_image)(*arr.add(i));
                let image_name = CStr::from_ptr((self.image_get_name)(img)).to_string_lossy();
                if image_name == assembly || image_name.strip_suffix(".dll") == Some(assembly) {
                    let ns = CString::new(ns)?;
                    let name_c = CString::new(name)?;
                    let cls = (self.class_from_name)(img, ns.as_ptr(), name_c.as_ptr());
                    ensure!(!cls.is_null(), "class {assembly}:{name} unavailable");
                    return Ok(cls);
                }
            }
        }
        anyhow::bail!("assembly {assembly} unavailable")
    }
    pub fn nested(&self, parent: *mut c_void, name: &str) -> Result<*mut c_void> {
        unsafe {
            let mut iter = core::ptr::null_mut();
            loop {
                let cls = (self.class_get_nested_types)(parent, &mut iter);
                ensure!(!cls.is_null(), "nested class {name} unavailable");
                if CStr::from_ptr((self.class_get_name)(cls)).to_bytes() == name.as_bytes() {
                    return Ok(cls);
                }
            }
        }
    }
    pub fn method(&self, cls: *mut c_void, name: &str, argc: i32) -> Result<*mut c_void> {
        let c = CString::new(name)?;
        let m = unsafe { (self.class_get_method_from_name)(cls, c.as_ptr(), argc) };
        ensure!(!m.is_null(), "method {name}/{argc} unavailable");
        Ok(m)
    }
    pub fn entry(&self, method: *mut c_void) -> Result<*mut c_void> {
        let p = unsafe { *method.cast::<*mut c_void>() };
        ensure!(!p.is_null(), "native entry unavailable");
        Ok(p)
    }
    pub fn field(&self, cls: *mut c_void, name: &str) -> Result<*mut c_void> {
        let c = CString::new(name)?;
        let f = unsafe { (self.class_get_field_from_name)(cls, c.as_ptr()) };
        ensure!(!f.is_null(), "field {name} unavailable");
        Ok(f)
    }
    pub fn offset(&self, cls: *mut c_void, name: &str) -> Result<usize> {
        let offset = unsafe { (self.field_get_offset)(self.field(cls, name)?) };
        ensure!(
            (16..4096).contains(&offset),
            "invalid instance field {name}"
        );
        Ok(offset)
    }
    pub fn string(&self, p: *mut c_void) -> Result<String> {
        ensure!(!p.is_null(), "null managed string");
        unsafe {
            let n = (self.string_length)(p);
            ensure!(
                (0..=64_000_000).contains(&n),
                "invalid managed string length"
            );
            let chars = (self.string_chars)(p);
            ensure!(!chars.is_null(), "null managed string chars");
            String::from_utf16(core::slice::from_raw_parts(chars, n as usize))
                .context("invalid UTF-16")
        }
    }
    pub fn new_string(&self, text: &str) -> Result<Root> {
        let text = CString::new(text)?;
        self.root(unsafe { (self.string_new)(text.as_ptr()) })
    }
    pub fn root(&self, obj: *mut c_void) -> Result<Root> {
        root_object(
            obj,
            self.gchandle_new,
            self.gchandle_free,
            self.gchandle_get_target,
        )
    }
    pub fn new_object(&self, cls: *mut c_void) -> Result<Root> {
        let obj = self.root(unsafe { (self.object_new)(cls) })?;
        self.invoke(self.method(cls, ".ctor", 0)?, obj.ptr(), &mut [])?;
        Ok(obj)
    }
    pub fn invoke(
        &self,
        method: *mut c_void,
        this: *mut c_void,
        args: &mut [*mut c_void],
    ) -> Result<*mut c_void> {
        let mut exception = core::ptr::null_mut();
        let result =
            unsafe { (self.runtime_invoke)(method, this, args.as_mut_ptr(), &mut exception) };
        // Method/type names distinguish ABI and construction failures without
        // logging exception messages, which may contain account/request data.
        if !exception.is_null() {
            let name = |p: *const i8| -> String {
                if p.is_null() {
                    "<unknown>".into()
                } else {
                    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
                }
            };
            let method_name = name(unsafe { (self.method_get_name)(method) });
            let exception_class = unsafe { (self.object_get_class)(exception) };
            let exception_type = if exception_class.is_null() {
                "<unknown>".into()
            } else {
                name(unsafe { (self.class_get_name)(exception_class) })
            };
            anyhow::bail!("managed method invocation failed: {method_name} ({exception_type})");
        }
        Ok(result)
    }
    pub fn get(&self, obj: *mut c_void, name: &str) -> Result<*mut c_void> {
        ensure!(!obj.is_null(), "null method receiver");
        self.invoke(
            self.method(unsafe { (self.object_get_class)(obj) }, name, 0)?,
            obj,
            &mut [],
        )
    }
    pub fn bool_value(&self, obj: *mut c_void) -> Result<bool> {
        ensure!(!obj.is_null(), "null boxed boolean");
        Ok(unsafe { *(self.object_unbox)(obj).cast::<u8>() != 0 })
    }
    pub fn read_ptr(&self, obj: *mut c_void, offset: usize) -> *mut c_void {
        unsafe { *obj.cast::<u8>().add(offset).cast::<*mut c_void>() }
    }
    pub fn write_ptr(&self, obj: *mut c_void, offset: usize, value: *mut c_void) {
        unsafe {
            (self.gc_wbarrier_set_field)(obj, obj.cast::<u8>().add(offset).cast(), value);
        }
    }
}

fn root_object(
    obj: *mut c_void,
    create: unsafe extern "C" fn(*mut c_void, bool) -> usize,
    free: unsafe extern "C" fn(usize),
    target: unsafe extern "C" fn(usize) -> *mut c_void,
) -> Result<Root> {
    ensure!(!obj.is_null(), "null managed object");
    let handle = unsafe { create(obj, false) };
    ensure!(handle != 0, "GC rooting failed");
    Ok(Root {
        handle,
        free,
        target,
    })
}

#[cfg(test)]
mod handle_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    const HANDLE: usize = 0x0000_0001_2345_6028;
    const OBJECT: usize = 0x4321;
    static FREED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn create(_: *mut c_void, _: bool) -> usize {
        HANDLE
    }
    unsafe extern "C" fn target(handle: usize) -> *mut c_void {
        if handle == HANDLE {
            OBJECT as _
        } else {
            core::ptr::null_mut()
        }
    }
    unsafe extern "C" fn free(handle: usize) {
        FREED.store(handle, Ordering::SeqCst);
    }
    #[test]
    fn native_pointer_handle_roundtrips_without_truncation() {
        FREED.store(0, Ordering::SeqCst);
        // Resolve the actual native pointer-width functions through the same
        // typed binding seam as GetProcAddress, including the return register.
        let root = root_object(
            OBJECT as _,
            unsafe {
                core::mem::transmute(create as unsafe extern "C" fn(*mut c_void, bool) -> usize)
            },
            unsafe { core::mem::transmute(free as unsafe extern "C" fn(usize)) },
            unsafe { core::mem::transmute(target as unsafe extern "C" fn(usize) -> *mut c_void) },
        )
        .unwrap();
        assert_eq!(
            root.ptr() as usize,
            OBJECT,
            "native GC handle lost its high 32 bits"
        );
        drop(root);
        assert_eq!(FREED.load(Ordering::SeqCst), HANDLE);
    }
}

fn assembly_count(
    corlib: unsafe extern "C" fn() -> *mut c_void,
    _domain_get: unsafe extern "C" fn() -> *mut c_void,
    assemblies: unsafe extern "C" fn(*mut c_void, *mut usize) -> *mut *mut c_void,
) -> usize {
    unsafe {
        // domain_get is a lazy allocator in the supported runtime. Before
        // corlib initialization it dereferences a null core Object class; only the
        // get_corlib global-pointer read is safe as the first readiness probe.
        if corlib().is_null() {
            return 0;
        }
        let mut n = 0;
        // The verified native domain_get_assemblies export ignores its domain
        // argument and reads the registered assembly vector. Do not allocate a
        // lazy root-domain object on an unattached initialization worker.
        let inventory = assemblies(core::ptr::null_mut(), &mut n);
        if inventory.is_null() || n > 100_000 {
            0
        } else {
            n
        }
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DOMAIN_CALLS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn cold_corlib() -> *mut c_void {
        core::ptr::null_mut()
    }
    unsafe extern "C" fn cold_domain() -> *mut c_void {
        DOMAIN_CALLS.fetch_add(1, Ordering::SeqCst);
        // The game's lazy domain constructor faults on a null class here.
        // Record the prohibited call instead of faulting the entire test host.
        core::ptr::null_mut()
    }
    unsafe extern "C" fn no_assemblies(_: *mut c_void, _: *mut usize) -> *mut *mut c_void {
        core::ptr::null_mut()
    }
    #[test]
    fn cold_runtime_probe_never_calls_lazy_domain_constructor() {
        DOMAIN_CALLS.store(0, Ordering::SeqCst);
        assert_eq!(assembly_count(cold_corlib, cold_domain, no_assemblies), 0);
        assert_eq!(
            DOMAIN_CALLS.load(Ordering::SeqCst),
            0,
            "startup called il2cpp_domain_get before corlib was ready (reported crash path)"
        );
    }
}

pub struct Root {
    // Unity 6's verified native handle is an address within an 8 KiB slot slab.
    // Its high bits are required by both get_target and free.
    handle: usize,
    free: unsafe extern "C" fn(usize),
    target: unsafe extern "C" fn(usize) -> *mut c_void,
}
impl Root {
    pub fn ptr(&self) -> *mut c_void {
        unsafe { (self.target)(self.handle) }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        unsafe {
            (self.free)(self.handle);
        }
    }
}
