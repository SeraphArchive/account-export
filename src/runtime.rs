use crate::il2cpp::{Il2Cpp, Root};
use crate::snapshot::Snapshot;
use anyhow::{ensure, Context, Result};
use core::ffi::c_void;
use retour::GenericDetour;
use serde_json::Value;
use std::cell::Cell;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HMODULE, TRUE};
use windows_sys::Win32::System::Environment::GetCommandLineW;
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::{CreateThread, ExitProcess};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
};

static API: OnceLock<Il2Cpp> = OnceLock::new();
static META: OnceLock<Meta> = OnceLock::new();
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static DATA: OnceLock<PathBuf> = OnceLock::new();
static READY: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static AUTH_UID: AtomicU64 = AtomicU64::new(0);

type Update = unsafe extern "C" fn(*mut c_void, f32, *mut c_void);
type Callback = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type Headers = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
type UShort = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut u16, *mut c_void) -> bool;
type Error = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void);
type Begin = unsafe extern "C" fn(*mut c_void, *mut c_void);
static UPDATE: OnceLock<GenericDetour<Update>> = OnceLock::new();
static CONFIRM: OnceLock<GenericDetour<Callback>> = OnceLock::new();
static PULL: OnceLock<GenericDetour<Callback>> = OnceLock::new();
static HEADERS: OnceLock<GenericDetour<Headers>> = OnceLock::new();
static SIGNED_HEADERS: OnceLock<GenericDetour<Headers>> = OnceLock::new();
static USHORT: OnceLock<GenericDetour<UShort>> = OnceLock::new();
static ERROR: OnceLock<GenericDetour<Error>> = OnceLock::new();
static BEGIN: OnceLock<GenericDetour<Begin>> = OnceLock::new();
static WAIT_TAP: OnceLock<GenericDetour<Begin>> = OnceLock::new();
static LOGIN: OnceLock<GenericDetour<Callback>> = OnceLock::new();

thread_local! { static READ_SCOPE: Cell<usize> = const { Cell::new(0) }; }

struct Meta {
    packet: usize,
    delta: usize,
    api: usize,
    deltas: usize,
    target: usize,
    request: usize,
    queue: usize,
    db: usize,
    model: usize,
    version: usize,
    raw_body: usize,
    raw_code: usize,
    uri: usize,
    signature_instance: usize,
    skip_tap: usize,
    payment_balance: usize,
    login_uid: usize,
    native_tap: usize,
}

#[derive(Clone, PartialEq)]
enum Phase {
    Start,
    Pull(usize),
    Balance,
    Verify,
    Done,
}
struct State {
    owner: Option<Root>,
    db: usize,
    snapshot: Snapshot,
    phase: Phase,
    batches: Vec<Vec<String>>,
    balance_task: Option<Root>,
    retries: usize,
    last_progress: Instant,
    signed_identity: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            owner: None,
            db: 0,
            snapshot: Snapshot::default(),
            phase: Phase::Start,
            batches: vec![],
            balance_task: None,
            retries: 0,
            last_progress: Instant::now(),
            signed_identity: false,
        }
    }
}

fn api() -> &'static Il2Cpp {
    API.get().unwrap()
}
fn meta() -> &'static Meta {
    META.get().unwrap()
}
fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.get().unwrap().lock().unwrap()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn log(text: &str) {
    if let Some(dir) = DATA.get() {
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("export.log"))
        {
            let _ = writeln!(f, "[{}] {text}", now());
        }
    }
}
fn popup(text: &str, error: bool) {
    let text = wide(text);
    let title = wide("HBR Account Export");
    unsafe {
        MessageBoxW(
            core::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK
                | if error {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}
fn fail(message: &str) {
    if !STOP.swap(true, Ordering::SeqCst) {
        log(message);
        let message = format!(
            "Account export failed: {message}\nNo complete archive was created. The game has been left open.\nPlease keep the log for troubleshooting, then restart the game."
        );
        let _ = std::thread::Builder::new()
            .name("export-error".into())
            .spawn(move || popup(&message, true));
    }
}

fn error_text(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| {
            if let Some(io) = cause.downcast_ref::<std::io::Error>() {
                // Windows may localize its error text. Codes/kinds keep diagnostics
                // in English without changing the user's system language.
                match io.raw_os_error() {
                    Some(code) => format!("I/O error ({:?}, OS code {code})", io.kind()),
                    None => format!("I/O error ({:?})", io.kind()),
                }
            } else {
                cause.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(": ")
}
fn guard<T>(fallback: T, f: impl FnOnce() -> Result<T>) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            fail(&error_text(&e));
            fallback
        }
        Err(_) => {
            fail("export hook panicked");
            fallback
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(hinst: HMODULE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DisableThreadLibraryCalls(hinst);
        // Stack-only process gating; CEF helpers also run this executable.
        // SDK threads can reserve only 64 KiB. DllMain's frame is allocated
        // even for DLL_THREAD_ATTACH, so the process-gating buffer must be small.
        let mut buf = [0u16; 520];
        let n =
            GetModuleFileNameW(core::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) as usize;
        if n == 0 || n >= buf.len() {
            return TRUE;
        }
        let start = buf[..n]
            .iter()
            .rposition(|c| *c == 92 || *c == 47)
            .map_or(0, |i| i + 1);
        let expected = b"HeavenBurnsRed.exe";
        if n - start != expected.len()
            || !buf[start..n]
                .iter()
                .zip(expected)
                .all(|(&u, &a)| u < 128 && (u as u8).eq_ignore_ascii_case(&a))
        {
            return TRUE;
        }
        let cmd = GetCommandLineW();
        if !cmd.is_null() {
            let mut len = 0;
            while len < 32768 && *cmd.add(len) != 0 {
                len += 1;
            }
            let units = core::slice::from_raw_parts(cmd, len);
            if units.windows(7).any(|w| {
                w == [45, 45, 116, 121, 112, 101, 61] || w == [45, 45, 116, 121, 112, 101, 32]
            }) {
                return TRUE;
            }
        }
        let thread = CreateThread(
            core::ptr::null(),
            0,
            Some(initialize),
            core::ptr::null(),
            0,
            core::ptr::null_mut(),
        );
        if !thread.is_null() {
            CloseHandle(thread);
        }
    }
    TRUE
}

unsafe extern "system" fn initialize(_: *mut c_void) -> u32 {
    guard(0, || {
        let mut buf = vec![0u16; 32768];
        let n = unsafe {
            GetModuleFileNameW(core::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32)
        } as usize;
        ensure!(
            n > 0 && n < buf.len(),
            "cannot resolve executable directory"
        );
        let exe = PathBuf::from(String::from_utf16(&buf[..n])?);
        let data = exe
            .parent()
            .context("executable has no parent")?
            .join("account-export");
        crate::storage::directory(&data)?;
        // Create a private log without following a pre-existing file link.
        let log_path = data.join("export.log");
        match std::fs::symlink_metadata(&log_path) {
            Ok(m) => {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    m.is_file() && m.file_attributes() & 0x400 == 0,
                    "unsafe log file"
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        DATA.set(data).ok();
        log(concat!(
            "standalone account export starting v",
            env!("CARGO_PKG_VERSION")
        ));
        let start = Instant::now();
        let binding = loop {
            if let Ok(binding) = Il2Cpp::load() {
                break binding;
            }
            ensure!(
                start.elapsed() < Duration::from_secs(120),
                "IL2CPP runtime unavailable"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        log("IL2CPP exports resolved; waiting for corlib");
        while unsafe { (binding.get_corlib)() }.is_null() {
            ensure!(
                start.elapsed() < Duration::from_secs(150),
                "IL2CPP corlib initialization timed out"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        log("corlib ready; waiting for stable assembly inventory");
        let mut previous = 0;
        let mut stable = 0;
        loop {
            ensure!(
                start.elapsed() < Duration::from_secs(150),
                "IL2CPP assembly initialization timed out"
            );
            let n = binding.count();
            if n >= 50 && n == previous {
                stable += 1;
            } else {
                stable = 0;
                previous = n;
            }
            if stable >= 20 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        log(&format!(
            "assembly inventory stable: {previous}; attaching initialization thread"
        ));
        let thread = unsafe { (binding.thread_attach)((binding.domain_get)()) };
        ensure!(!thread.is_null(), "IL2CPP thread attach failed");
        API.set(binding).ok();
        log("initialization thread attached; resolving export metadata");
        let result = install();
        unsafe {
            (api().thread_detach)(thread);
        }
        result?;
        log("export hooks ready; waiting for authenticated UID");
        // Independent watchdog catches stalled requests even if Unity stops updates.
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if STOP.load(Ordering::SeqCst) {
                break;
            }
            let s = state();
            if s.phase == Phase::Done {
                break;
            }
            if s.owner.is_some() && s.last_progress.elapsed() > Duration::from_secs(180) {
                drop(s);
                fail("export request timed out");
                break;
            }
        }
        Ok(0)
    })
}

fn install() -> Result<()> {
    let a = api();
    let controller = a.class("DeltaComm", "DeltaComm", "DeltaCommController")?;
    let packet = a.nested(controller, "Packet")?;
    let delta = a.nested(controller, "Delta")?;
    let raw = a.class("HTTPComm", "HTTPComm", "RawSuccessResponsePayload")?;
    let uwr = a.class("HTTPCommGenerated", "HTTPComm", "UWRServerRequest")?;
    let signature = a.class("HTTPComm", "HTTPComm", "SignatureProvider")?;
    let prologue = a.class("Assembly-CSharp", "Lily", "Prologue")?;
    // SDK assembly name is resolved from the original image inventory.
    let payment = a.class("WFS.Gamelib.Payment", "GreeGameLib", "Payment")?;
    let login = a.class("GGLSequences", "GGLSequences", "Prologue")?;
    let login_callback = a.nested(login, "<>c__DisplayClass62_1")?;
    let login_payload = a.nested(login, "GDPRResponse")?;
    let m = Meta {
        packet: packet as usize,
        delta: delta as usize,
        api: a.offset(packet, "api")?,
        deltas: a.offset(packet, "deltas")?,
        target: a.offset(delta, "target")?,
        request: a.offset(controller, "_request")?,
        queue: a.offset(controller, "_volatilePackets1")?,
        db: a.offset(controller, "_db")?,
        model: a.offset(controller, "_model")?,
        version: a.offset(controller, "_clientVersion")?,
        raw_body: a.offset(raw, "body")?,
        raw_code: a.offset(raw, "code")?,
        uri: a.offset(uwr, "m_uri")?,
        signature_instance: a.method(signature, "get_Instance", 0)? as usize,
        skip_tap: a.field(prologue, "skip_tap")? as usize,
        payment_balance: a.method(payment, "queryBalanceAsync", 1)? as usize,
        login_uid: a.offset(login_payload, "userId")?,
        native_tap: a.method(login, "TapToStart", 0)? as usize,
    };
    // Resolve every required target before installing anything.
    let native_update = a.entry(a.method(controller, "Update", 1)?)?;
    let confirm = a.entry(a.method(controller, "<Update>b__40_0", 1)?)?;
    let pull = a.entry(a.method(controller, "<Update>b__40_5", 1)?)?;
    let error = a.entry(a.method(controller, "OnError", 2)?)?;
    let headers = a.entry(a.method(uwr, "PrepareHeaders", 1)?)?;
    let signed_headers = a.entry(a.method(signature, "GenerateHeaderCollection", 1)?)?;
    let tap = a.class("Assembly-CSharp", "Lily.Title", "TapToStartState")?;
    let begin = a.entry(a.method(tap, "OnStateBegin", 0)?)?;
    let logged_in = a.entry(a.method(login_callback, "<GGLQueryXUID_Init>b__2", 1)?)?;
    let wait_tap = a.entry(a.method(login, "WaitTapToStart_Proc", 0)?)?;
    META.set(m).ok();
    STATE.set(Mutex::new(State::default())).ok();
    unsafe {
        crate::hook::install(
            &CONFIRM,
            core::mem::transmute::<_, Callback>(confirm),
            confirm_callback,
        )?;
        crate::hook::install(
            &PULL,
            core::mem::transmute::<_, Callback>(pull),
            pull_callback,
        )?;
        crate::hook::install(&ERROR, core::mem::transmute::<_, Error>(error), on_error)?;
        crate::hook::install(
            &HEADERS,
            core::mem::transmute::<_, Headers>(headers),
            prepare_headers,
        )?;
        crate::hook::install(
            &SIGNED_HEADERS,
            core::mem::transmute::<_, Headers>(signed_headers),
            generated_headers,
        )?;
        crate::hook::install(&BEGIN, core::mem::transmute::<_, Begin>(begin), tap_begin)?;
        crate::hook::install(
            &LOGIN,
            core::mem::transmute::<_, Callback>(logged_in),
            login_success,
        )?;
        crate::hook::install(
            &WAIT_TAP,
            core::mem::transmute::<_, Begin>(wait_tap),
            wait_tap_proc,
        )?;
        // Update is last: no pending push can run through a partially set-up exporter.
        crate::hook::install(
            &UPDATE,
            core::mem::transmute::<_, Update>(native_update),
            update,
        )?;
    }
    READY.store(true, Ordering::SeqCst);
    Ok(())
}

fn current_uid() -> Result<String> {
    let instance = api().invoke(
        meta().signature_instance as _,
        core::ptr::null_mut(),
        &mut [],
    )?;
    if instance.is_null() {
        return Ok(String::new());
    }
    let uid = api().get(instance, "XUID")?;
    if uid.is_null() {
        return Ok(String::new());
    }
    api().string(uid)
}

unsafe extern "C" fn login_success(this: *mut c_void, payload: *mut c_void, method: *mut c_void) {
    LOGIN.get().unwrap().call(this, payload, method);
    guard((), || {
        ensure!(!payload.is_null(), "null login response");
        let uid = unsafe { *payload.cast::<u8>().add(meta().login_uid).cast::<u64>() };
        ensure!(
            uid > 0 && uid <= i64::MAX as u64,
            "login did not return a valid UID"
        );
        let previous = AUTH_UID.swap(uid, Ordering::SeqCst);
        ensure!(
            previous == 0 || previous == uid,
            "authenticated account changed during export"
        );
        log("successful server login observed");
        Ok(())
    });
}

unsafe extern "C" fn tap_begin(this: *mut c_void, method: *mut c_void) {
    // The game has a native skip_tap path with its own UI/readiness checks.
    // A saved UID permits automating the tap, never exporting before live login.
    if READY.load(Ordering::SeqCst) && !STOP.load(Ordering::SeqCst) {
        guard((), || {
            let uid = current_uid()?;
            if uid.parse::<u64>().is_ok_and(|u| u > 0) {
                let value: u8 = 1;
                unsafe {
                    (api().field_static_set_value)(
                        meta().skip_tap as _,
                        (&value as *const u8).cast_mut().cast(),
                    );
                }
            }
            Ok(())
        });
    }
    BEGIN.get().unwrap().call(this, method);
}

unsafe extern "C" fn wait_tap_proc(this: *mut c_void, method: *mut c_void) {
    if READY.load(Ordering::SeqCst) && !STOP.load(Ordering::SeqCst) {
        guard((), || {
            if current_uid()?.parse::<u64>().is_ok_and(|u| u > 0) {
                api().invoke(meta().native_tap as _, this, &mut [])?;
            }
            Ok(())
        });
    }
    WAIT_TAP.get().unwrap().call(this, method);
}

unsafe extern "C" fn update(this: *mut c_void, _: f32, method: *mut c_void) {
    if !READY.load(Ordering::SeqCst) || STOP.load(Ordering::SeqCst) {
        return;
    }
    guard((), || update_inner(this, method));
}

fn update_inner(this: *mut c_void, method: *mut c_void) -> Result<()> {
    let a = api();
    let m = meta();
    if a.read_ptr(this, m.model).is_null() {
        return Ok(());
    }
    if AUTH_UID.load(Ordering::SeqCst) == 0 {
        return Ok(());
    }
    let uid = current_uid()?;
    ensure!(
        uid.parse::<u64>().ok() == Some(AUTH_UID.load(Ordering::SeqCst)),
        "logged-in UID does not match login response"
    );
    {
        let s = state();
        if let Some(owner) = &s.owner {
            if owner.ptr() != this {
                return Ok(());
            }
        }
    }
    let version = a.string(a.read_ptr(this, m.version))?;
    let fresh = state().owner.is_none();
    if fresh {
        let db = a.read_ptr(this, m.db);
        ensure!(!db.is_null(), "save DB unavailable");
        let db_class = unsafe { (a.object_get_class)(db) };
        let entry = a.entry(a.method(db_class, "GetUShort", 2)?)?;
        unsafe {
            crate::hook::install(
                &USHORT,
                core::mem::transmute::<_, UShort>(entry),
                get_ushort,
            )?;
        }
        let owner = a.root(this)?;
        let mut s = state();
        s.owner = Some(owner);
        s.db = db as usize;
        s.last_progress = Instant::now();
        log("authenticated; starting full server snapshot");
    }
    state().snapshot.identity(&uid, &version)?;
    let phase = state().phase.clone();
    if phase == Phase::Done {
        return Ok(());
    }
    if phase == Phase::Balance {
        return poll_balance();
    }
    if !a.read_ptr(this, m.request).is_null() {
        return Ok(());
    }

    // Update inlines LoadFirstPacket. A temporary List<Packet> plus a scoped
    // zero/zero queue-index read is required to select our read-only packet.
    // The real DB and the original volatile/persisted queues are retained.
    let packet = a.new_object(m.packet as _)?;
    let route = a.new_string(if matches!(phase, Phase::Pull(_)) {
        "pull"
    } else {
        "confirm"
    })?;
    a.write_ptr(packet.ptr(), m.api, route.ptr());
    if let Phase::Pull(index) = phase {
        let names = state().batches[index].clone();
        let array = a.root(unsafe { (a.array_new)(m.delta as _, names.len()) })?;
        for (i, name) in names.iter().enumerate() {
            let delta = a.new_object(m.delta as _)?;
            let name = a.new_string(name)?;
            a.write_ptr(delta.ptr(), m.target, name.ptr());
            // x64 Il2CppArray header: object, bounds, length, then references.
            a.write_ptr(array.ptr(), 0x20 + i * 8, delta.ptr());
        }
        a.write_ptr(packet.ptr(), m.deltas, array.ptr());
    }
    let original = a.root(a.read_ptr(this, m.queue))?;
    let list_class = unsafe { (a.object_get_class)(original.ptr()) };
    let list = a.new_object(list_class)?;
    a.invoke(
        a.method(list_class, "Add", 1)?,
        list.ptr(),
        &mut [packet.ptr()],
    )?;
    struct Restore {
        this: usize,
        original: Root,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            api().write_ptr(self.this as _, meta().queue, self.original.ptr());
            READ_SCOPE.with(|s| s.set(0));
        }
    }
    let _restore = Restore {
        this: this as usize,
        original,
    };
    a.write_ptr(this, m.queue, list.ptr());
    state().signed_identity = false;
    READ_SCOPE.with(|s| s.set(state().db));
    unsafe {
        UPDATE.get().unwrap().call(this, 0.0, method);
    }
    Ok(())
}

unsafe extern "C" fn get_ushort(
    this: *mut c_void,
    key: *mut c_void,
    out: *mut u16,
    method: *mut c_void,
) -> bool {
    if READ_SCOPE.with(|s| s.get() == this as usize && s.get() != 0) {
        return guard(false, || {
            let key_text = api().string(key)?;
            if key_text == "id0" || key_text == "id1" {
                ensure!(!out.is_null(), "null queue index output");
                unsafe {
                    *out = 0;
                }
                Ok(true)
            } else {
                Ok(unsafe { USHORT.get().unwrap().call(this, key, out, method) })
            }
        });
    }
    USHORT.get().unwrap().call(this, key, out, method)
}

unsafe extern "C" fn prepare_headers(
    this: *mut c_void,
    headers: *mut c_void,
    method: *mut c_void,
) -> *mut c_void {
    let result = HEADERS.get().unwrap().call(this, headers, method);
    if !READY.load(Ordering::SeqCst) || STOP.load(Ordering::SeqCst) {
        return result;
    }
    guard((), || {
        let owner = state().owner.as_ref().map(|r| r.ptr());
        let Some(owner) = owner else {
            return Ok(());
        };
        if api().read_ptr(owner, meta().request) != this {
            return Ok(());
        }
        ensure!(!result.is_null(), "export headers unavailable");
        let uri = api().read_ptr(this, meta().uri);
        let path = api().string(api().get(uri, "get_AbsolutePath")?)?;
        ensure!(
            path == "/api/user/confirm" || path == "/api/user/pull",
            "unexpected export request route"
        );
        // This dictionary contains request/base headers only. The actual
        // x-player-id is generated later by the signature provider.
        state().snapshot.identity(&current_uid()?, "")?;
        Ok(())
    });
    result
}

unsafe extern "C" fn generated_headers(
    this: *mut c_void,
    transient_uid: *mut c_void,
    method: *mut c_void,
) -> *mut c_void {
    let result = SIGNED_HEADERS
        .get()
        .unwrap()
        .call(this, transient_uid, method);
    if !READY.load(Ordering::SeqCst) || STOP.load(Ordering::SeqCst) {
        return result;
    }
    guard((), || {
        if state().snapshot.uid.is_none() {
            return Ok(());
        }
        // Platform login has its own transient identity. Export packets use
        // the authenticated provider UID with no transient override.
        if !transient_uid.is_null() && !api().string(transient_uid)?.is_empty() {
            return Ok(());
        }
        ensure!(!result.is_null(), "signed export headers unavailable");
        let cls = unsafe { (api().object_get_class)(result) };
        let get = api().method(cls, "get_Item", 1)?;
        let key = api().new_string("x-player-id")?;
        let uid = api().string(api().invoke(get, result, &mut [key.ptr()])?)?;
        let mut s = state();
        s.snapshot.identity(&uid, "")?;
        s.signed_identity = true;
        Ok(())
    });
    result
}

fn response(this: *mut c_void, payload: *mut c_void) -> Result<Value> {
    ensure!(!payload.is_null(), "null response payload");
    let owner = state().owner.as_ref().map(|r| r.ptr());
    ensure!(owner == Some(this), "response controller changed");
    ensure!(
        state().signed_identity,
        "export response arrived without verified signed identity"
    );
    ensure!(
        unsafe { *payload.cast::<u8>().add(meta().raw_code).cast::<i32>() } == 0,
        "server response failed"
    );
    let body = api().string(api().read_ptr(payload, meta().raw_body))?;
    let value =
        serde_json::from_str(&body).map_err(|_| anyhow::anyhow!("invalid response JSON"))?;
    // Check authenticated identity again before accepting the response.
    state().snapshot.identity(&current_uid()?, "")?;
    api().write_ptr(this, meta().request, core::ptr::null_mut());
    state().last_progress = Instant::now();
    Ok(value)
}

unsafe extern "C" fn confirm_callback(this: *mut c_void, payload: *mut c_void, _: *mut c_void) {
    if STOP.load(Ordering::SeqCst) {
        return;
    }
    guard((), || {
        let body = response(this, payload)?;
        let phase = state().phase.clone();
        match phase {
            Phase::Start => {
                let mut s = state();
                s.snapshot.begin(&body, now())?;
                s.batches = s.snapshot.batches();
                s.phase = Phase::Pull(0);
                log(&format!(
                    "full pull queued: {} tables / {} chunks",
                    s.batches.iter().map(Vec::len).sum::<usize>(),
                    s.batches.len()
                ));
            }
            Phase::Verify => {
                crate::snapshot::success(&body)?;
                let verified = state().snapshot.verify(&body);
                if verified.is_err() {
                    let mut s = state();
                    ensure!(
                        s.retries < 2,
                        "server snapshot did not stabilize after three attempts"
                    );
                    s.retries += 1;
                    s.snapshot.begin(&body, now())?;
                    s.snapshot.balance = None;
                    s.batches = s.snapshot.batches();
                    s.phase = Phase::Pull(0);
                    log("server changed; restarting full read");
                    return Ok(());
                }
                let doc = state().snapshot.archive(now())?;
                let uid = state().snapshot.uid.unwrap();
                state().phase = Phase::Done;
                std::thread::Builder::new()
                    .name("export-writer".into())
                    .spawn(move || finish(doc, uid))?;
            }
            _ => anyhow::bail!("unexpected confirm response"),
        }
        Ok(())
    });
}

unsafe extern "C" fn pull_callback(this: *mut c_void, payload: *mut c_void, _: *mut c_void) {
    if STOP.load(Ordering::SeqCst) {
        return;
    }
    guard((), || {
        let body = response(this, payload)?;
        let mut s = state();
        let Phase::Pull(index) = s.phase else {
            anyhow::bail!("unexpected pull response");
        };
        let names = s.batches[index].clone();
        s.snapshot.pull(&names, &body)?;
        s.batches[index].retain(|name| body["tables"].get(name).is_none());
        s.phase = if !s.batches[index].is_empty() {
            Phase::Pull(index)
        } else if index + 1 == s.batches.len() {
            Phase::Balance
        } else {
            Phase::Pull(index + 1)
        };
        log(&format!(
            "full pull received: {} chunks, {} tables still pending",
            s.snapshot.pull_chunks,
            s.batches.iter().map(Vec::len).sum::<usize>()
        ));
        Ok(())
    });
}

fn poll_balance() -> Result<()> {
    let task = state().balance_task.take();
    let Some(task) = task else {
        let mut days: i32 = -1;
        let obj = api().invoke(
            meta().payment_balance as _,
            core::ptr::null_mut(),
            &mut [(&mut days as *mut i32).cast()],
        )?;
        state().balance_task = Some(api().root(obj)?);
        log("querying payment balance");
        return Ok(());
    };
    if !api().bool_value(api().get(task.ptr(), "get_IsCompleted")?)? {
        state().balance_task = Some(task);
        return Ok(());
    }
    ensure!(
        !api().bool_value(api().get(task.ptr(), "get_IsFaulted")?)?
            && !api().bool_value(api().get(task.ptr(), "get_IsCanceled")?)?,
        "payment balance task failed"
    );
    let result = api().root(api().get(task.ptr(), "get_Result")?)?;
    ensure!(
        api().get(result.ptr(), "get_Error")?.is_null(),
        "payment balance query rejected"
    );
    let balance = api().root(api().get(result.ptr(), "get_ResultObject")?)?;
    let read = |name| -> Result<i32> {
        let value = api().get(balance.ptr(), name)?;
        ensure!(!value.is_null(), "null balance value");
        Ok(unsafe { *(api().object_unbox)(value).cast::<i32>() })
    };
    state()
        .snapshot
        .set_balance(read("getFree")?, read("getCharge")?, read("getTotal")?)?;
    let mut s = state();
    s.phase = Phase::Verify;
    s.last_progress = Instant::now();
    log("payment balance received; confirming snapshot versions");
    Ok(())
}

unsafe extern "C" fn on_error(this: *mut c_void, code: i32, obj: *mut c_void, method: *mut c_void) {
    if READY.load(Ordering::SeqCst)
        && guard(true, || {
            Ok(state().owner.as_ref().is_some_and(|r| r.ptr() == this))
        })
    {
        fail(&format!("DeltaComm read failed (code {code})"));
        return;
    }
    ERROR.get().unwrap().call(this, code, obj, method);
}

fn finish(doc: Value, uid: u64) {
    guard((), || {
        let timestamp = now();
        let dest = crate::storage::write_archive(
            &DATA.get().unwrap().join("exports"),
            &doc,
            uid,
            timestamp,
        )?;
        log("full archive committed and readback verified");
        let message = format!(
            "Full account export completed for account {uid}.\nFile: {}\nTables: {}  Rows: {}",
            dest.display(),
            doc["stats"]["tableCount"],
            doc["stats"]["rowCount"]
        );
        if let Err(error) = spawn_notice(&message) {
            STOP.store(true, Ordering::SeqCst);
            log("archive retained; completion notifier could not initialize");
            popup(
                &format!(
                    "The archive was saved successfully:\n{}\nAutomatic exit / completion notification failed: {}\nPlease close the game manually.",
                    dest.display(), error_text(&error)
                ),
                true,
            );
            return Ok(());
        }
        // The independent notifier waits for this exact process to exit, then
        // displays the result. No Unity shutdown push is allowed after export.
        unsafe {
            ExitProcess(0);
        }
    });
}

fn spawn_notice(message: &str) -> Result<()> {
    use base64::Engine;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_sys::Win32::System::Threading::{
        CreateEventW, CreateProcessW, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    let pid = std::process::id();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let event_name = format!("Local\\HbrExportNotice-{pid}-{nonce}");
    let event_wide = wide(&event_name);
    let event = unsafe { CreateEventW(core::ptr::null(), 1, 0, event_wide.as_ptr()) };
    ensure!(
        !event.is_null(),
        "cannot create notification readiness event"
    );
    struct Event(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Event {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    let _event_owner = Event(event);
    // Resolve UI and retain the exact game process handle before acknowledging
    // readiness. The parent never exits merely because CreateProcess succeeded.
    let script = notice_script(message, pid, &event_name);
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut system = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    ensure!(n > 0 && n < system.len(), "cannot locate system directory");
    let exe = format!(
        "{}\\WindowsPowerShell\\v1.0\\powershell.exe",
        String::from_utf16(&system[..n])?
    );
    let exe_wide = wide(&exe);
    let mut command = wide(&format!(
        "\"{exe}\" -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}"
    ));
    let mut startup: STARTUPINFOW = unsafe { core::mem::zeroed() };
    startup.cb = core::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { core::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            exe_wide.as_ptr(),
            command.as_mut_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            core::ptr::null(),
            core::ptr::null(),
            &startup,
            &mut process,
        )
    };
    ensure!(
        ok != 0,
        "cannot start export completion notifier; archive retained"
    );
    let ready = unsafe { WaitForSingleObject(event, 15_000) };
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    ensure!(
        ready == 0,
        "completion notifier did not become ready; archive retained"
    );
    Ok(())
}

fn notice_script(message: &str, pid: u32, event_name: &str) -> String {
    let message = message.replace('\'', "''");
    format!(
        r#"$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class HbrExportNotice {{ [DllImport("kernel32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr OpenEvent(uint access, bool inherit, string name); [DllImport("kernel32.dll")] public static extern bool SetEvent(IntPtr handle); [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle); }}'
$game = [Diagnostics.Process]::GetProcessById({pid})
$null = $game.Handle
$event = [HbrExportNotice]::OpenEvent(2, $false, '{event_name}')
if ($event -eq [IntPtr]::Zero) {{ throw 'Readiness event unavailable' }}
if (-not [HbrExportNotice]::SetEvent($event)) {{ throw 'Cannot acknowledge readiness' }}
[HbrExportNotice]::CloseHandle($event) | Out-Null
$game.WaitForExit()
[System.Windows.Forms.MessageBox]::Show('{message}','HBR Account Export') | Out-Null
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use windows_sys::Win32::System::Threading::{
        CreateEventW, WaitForSingleObject, CREATE_NO_WINDOW,
    };

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn notifier_acknowledges_readiness_and_waits_for_exact_process_exit() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let event_name = format!("Local\\HbrExportNoticeTest-{}-{nonce}", std::process::id());
        let event = unsafe { CreateEventW(core::ptr::null(), 1, 0, wide(&event_name).as_ptr()) };
        assert!(!event.is_null());
        struct Event(windows_sys::Win32::Foundation::HANDLE);
        impl Drop for Event {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let _event_owner = Event(event);
        let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut sleeper = ChildGuard(
            Command::new(&exe)
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 60",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let marker = std::env::temp_dir().join(format!("hbr-export-notice-{nonce}.txt"));
        let original = notice_script(
            "English notice with an apostrophe ' and Unicode character λ.",
            sleeper.0.id(),
            &event_name,
        );
        let mut script = original
            .lines()
            .take_while(|line| !line.starts_with("[System.Windows.Forms.MessageBox]"))
            .collect::<Vec<_>>()
            .join("\n");
        // Exercise the real UI initialization and handshake without opening a
        // dialog. Only replace its final rendering with a completion marker.
        script.push_str(&format!(
            "\n[IO.File]::WriteAllText('{}','done')",
            marker.display().to_string().replace('\'', "''")
        ));
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut notifier = ChildGuard(
            Command::new(exe)
                .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert_eq!(
            unsafe { WaitForSingleObject(event, 15_000) },
            0,
            "notifier did not initialize"
        );
        assert!(!marker.exists(), "completion happened before process exit");
        sleeper.0.kill().unwrap();
        sleeper.0.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while notifier.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "notifier did not finish after process exit"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "done");
        std::fs::remove_file(marker).unwrap();
    }
}
