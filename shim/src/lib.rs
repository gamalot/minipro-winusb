//! WinUSB shim for the official MiniPro (TL866CS) v6.85 software.
//!
//! MiniPro.exe talks to its programmer through a tiny KMDF driver ("MiniProWdf", 2012) that
//! Windows 11 now refuses to load.  The application's whole USB layer is:
//!
//!   * `SetupDiGetClassDevsA(<old interface GUID>)` + `SetupDiEnumDeviceInterfaces` to find devices,
//!   * `RegisterDeviceNotificationA(<old interface GUID>)` for hot-plug,
//!   * `CreateFileA(<device path>)` to open a device,
//!   * `DeviceIoControl(h, 0x222000, data, len, ...)`  = bulk OUT write  (<= 0xC00 bytes, 1.5 s timeout),
//!   * `DeviceIoControl(h, 0x222004, &timeout_ms, 4, buf, len, &n, 0)` = bulk IN read (<= 0xC00 bytes),
//!   * `CloseHandle`.
//!
//! The old driver picks, among the first two pipes of the interface, the bulk IN and the bulk OUT pipe.
//! This DLL is injected into MiniPro.exe, patches those five imports and reimplements the two IOCTLs
//! on top of WinUSB, so the device can be bound to the Microsoft-signed WinUSB driver instead.

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::ffi::c_void;
use std::io::Write;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

mod headless;

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::Usb::{
    WinUsb_Free, WinUsb_Initialize, WinUsb_QueryPipe, WinUsb_ReadPipe, WinUsb_SetPipePolicy,
    WinUsb_WritePipe, WINUSB_PIPE_INFORMATION, PIPE_TRANSFER_TIMEOUT,
};
use windows_sys::Win32::Foundation::{CloseHandle, SetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows_sys::Win32::System::Memory::{VirtualProtect, PAGE_READWRITE};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

// ---------------------------------------------------------------------------------------------
// constants taken from the old driver / application
// ---------------------------------------------------------------------------------------------

const IOCTL_WRITE: u32 = 0x0022_2000;
const IOCTL_READ: u32 = 0x0022_2004;
const MAX_XFER: u32 = 0xC00;
const WRITE_TIMEOUT_MS: u32 = 1500;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const ERROR_INVALID_FUNCTION: u32 = 1;
const ERROR_INVALID_PARAMETER: u32 = 87;

/// Interface GUID the application asks for (class GUID of the old MiniProWdf INF).
const OLD_GUID: GUID = GUID {
    data1: 0x8598_0D83,
    data2: 0x32B9,
    data3: 0x4BA1,
    data4: [0x8F, 0xDF, 0x12, 0xA7, 0x11, 0xB9, 0x9C, 0xA2],
};

/// Fallback WinUSB interface GUID (what Zadig generated on this machine).  The real one is read
/// from the registry at start-up, because Zadig picks a new random GUID on every (re)install.
const DEFAULT_NEW_GUID: GUID = GUID {
    data1: 0xD1FF_4FD3,
    data2: 0x1DF7,
    data3: 0x42A7,
    data4: [0x84, 0x8D, 0xA6, 0xF4, 0xDA, 0x31, 0x0D, 0x26],
};

// ---------------------------------------------------------------------------------------------
// state
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Dev {
    file: usize,
    iface: usize,
    pipe_in: u8,
    pipe_out: u8,
}

static DEVS: Mutex<Vec<Dev>> = Mutex::new(Vec::new());
static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);
pub(crate) static T0: OnceLock<Instant> = OnceLock::new();
static NEW_GUID: OnceLock<GUID> = OnceLock::new();

/// USB transfer statistics, used by the front-end to compute real progress.
pub(crate) static BYTES_IN: AtomicU64 = AtomicU64::new(0);
pub(crate) static BYTES_OUT: AtomicU64 = AtomicU64::new(0);
pub(crate) static OPS_IN: AtomicU64 = AtomicU64::new(0);
pub(crate) static OPS_OUT: AtomicU64 = AtomicU64::new(0);
pub(crate) static LAST_XFER_MS: AtomicU64 = AtomicU64::new(0);

fn note_transfer(read: bool, n: u32) {
    let ms = T0.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
    LAST_XFER_MS.store(ms, Ordering::Relaxed);
    if read {
        BYTES_IN.fetch_add(n as u64, Ordering::Relaxed);
        OPS_IN.fetch_add(1, Ordering::Relaxed);
    } else {
        BYTES_OUT.fetch_add(n as u64, Ordering::Relaxed);
        OPS_OUT.fetch_add(1, Ordering::Relaxed);
    }
}

static ORIG_CREATE_FILE: AtomicUsize = AtomicUsize::new(0);
static ORIG_IOCTL: AtomicUsize = AtomicUsize::new(0);
static ORIG_CLOSE: AtomicUsize = AtomicUsize::new(0);
static ORIG_SETUPDI: AtomicUsize = AtomicUsize::new(0);
static ORIG_REGNOTIFY: AtomicUsize = AtomicUsize::new(0);
static ORIG_ENUMIF: AtomicUsize = AtomicUsize::new(0);

type CreateFileAFn =
    unsafe extern "system" fn(*const u8, u32, u32, *const c_void, u32, u32, *mut c_void) -> *mut c_void;
type DeviceIoControlFn = unsafe extern "system" fn(
    *mut c_void, u32, *const c_void, u32, *mut c_void, u32, *mut u32, *mut c_void,
) -> i32;
type CloseHandleFn = unsafe extern "system" fn(*mut c_void) -> i32;
type SetupDiGetClassDevsAFn =
    unsafe extern "system" fn(*const GUID, *const u8, *mut c_void, u32) -> *mut c_void;
type SetupDiEnumDeviceInterfacesFn =
    unsafe extern "system" fn(*mut c_void, *const c_void, *const GUID, u32, *mut c_void) -> i32;
type RegisterDeviceNotificationAFn =
    unsafe extern "system" fn(*mut c_void, *const c_void, u32) -> *mut c_void;

// ---------------------------------------------------------------------------------------------
// logging
// ---------------------------------------------------------------------------------------------

pub(crate) fn log(msg: &str) {
    if let Ok(mut g) = LOG.lock() {
        if let Some(f) = g.as_mut() {
            let ms = T0.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
            let _ = writeln!(f, "[{ms:>8}] {msg}");
            let _ = f.flush();
        }
    }
}

macro_rules! log {
    ($($a:tt)*) => { log(&format!($($a)*)) };
}

fn hex(p: *const u8, n: usize) -> String {
    let n = n.min(16);
    (0..n)
        .map(|i| format!("{:02x}", unsafe { *p.add(i) }))
        .collect::<Vec<_>>()
        .join(" ")
}

unsafe fn open_log() {
    let mut h: *mut c_void = std::ptr::null_mut();
    let addr = DllMain as *const () as usize as *const u16;
    if GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        addr,
        &mut h,
    ) == 0
    {
        return;
    }
    let mut buf = [0u16; 520];
    let n = GetModuleFileNameW(h, buf.as_mut_ptr(), buf.len() as u32) as usize;
    if n == 0 {
        return;
    }
    let path = String::from_utf16_lossy(&buf[..n]);
    let log_path = format!("{}.log", path.trim_end_matches(".dll"));
    if let Ok(f) = std::fs::OpenOptions::new().create(true).write(true).truncate(true).open(&log_path) {
        *LOG.lock().unwrap() = Some(f);
    }
}

// ---------------------------------------------------------------------------------------------
// WinUSB interface GUID discovery
// ---------------------------------------------------------------------------------------------

fn parse_guid(s: &str) -> Option<GUID> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 || parts[0].len() != 8 || parts[1].len() != 4 || parts[2].len() != 4
        || parts[3].len() != 4 || parts[4].len() != 12
    {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    let tail = format!("{}{}", parts[3], parts[4]);
    let mut d4 = [0u8; 8];
    for (i, b) in d4.iter_mut().enumerate() {
        *b = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(GUID { data1: d1, data2: d2, data3: d3, data4: d4 })
}

/// Read DeviceInterfaceGUIDs of the present VID_04D8&PID_E11C device from the registry
/// (written there by the WinUSB INF that Zadig installs).
fn guid_from_registry() -> Option<GUID> {
    // Use reg.exe-free approach: windows-sys registry API would need another feature, and
    // a tiny dynamic lookup keeps the dependency surface small.
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    unsafe {
        let adv = LoadLibraryA(b"advapi32.dll\0".as_ptr());
        if adv.is_null() {
            return None;
        }
        type RegOpenKeyExW = unsafe extern "system" fn(isize, *const u16, u32, u32, *mut isize) -> i32;
        type RegEnumKeyExW = unsafe extern "system" fn(
            isize, u32, *mut u16, *mut u32, *mut u32, *mut u16, *mut u32, *mut c_void,
        ) -> i32;
        type RegQueryValueExW =
            unsafe extern "system" fn(isize, *const u16, *mut u32, *mut u32, *mut u8, *mut u32) -> i32;
        type RegCloseKey = unsafe extern "system" fn(isize) -> i32;
        let open: RegOpenKeyExW = std::mem::transmute(GetProcAddress(adv, b"RegOpenKeyExW\0".as_ptr())?);
        let enumk: RegEnumKeyExW = std::mem::transmute(GetProcAddress(adv, b"RegEnumKeyExW\0".as_ptr())?);
        let query: RegQueryValueExW =
            std::mem::transmute(GetProcAddress(adv, b"RegQueryValueExW\0".as_ptr())?);
        let close: RegCloseKey = std::mem::transmute(GetProcAddress(adv, b"RegCloseKey\0".as_ptr())?);

        let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        const HKLM: isize = 0x8000_0002u32 as i32 as isize;
        const KEY_READ: u32 = 0x20019;
        let base = r"SYSTEM\CurrentControlSet\Enum\USB\VID_04D8&PID_E11C";
        let mut hk = 0isize;
        if open(HKLM, w(base).as_ptr(), 0, KEY_READ, &mut hk) != 0 {
            return None;
        }
        let mut found = None;
        let mut idx = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if enumk(hk, idx, name.as_mut_ptr(), &mut len, std::ptr::null_mut(),
                     std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                break;
            }
            idx += 1;
            let sub = format!("{base}\\{}\\Device Parameters", String::from_utf16_lossy(&name[..len as usize]));
            let mut hs = 0isize;
            if open(HKLM, w(&sub).as_ptr(), 0, KEY_READ, &mut hs) != 0 {
                continue;
            }
            let mut ty = 0u32;
            let mut data = [0u8; 512];
            let mut sz = data.len() as u32;
            let r = query(hs, w("DeviceInterfaceGUIDs").as_ptr(), std::ptr::null_mut(), &mut ty, data.as_mut_ptr(), &mut sz);
            close(hs);
            if r == 0 && sz >= 2 {
                let u16s: Vec<u16> = data[..sz as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                let first = String::from_utf16_lossy(&u16s);
                if let Some(g) = parse_guid(first.split('\0').next().unwrap_or("")) {
                    found = Some(g);
                    break;
                }
            }
        }
        close(hk);
        found
    }
}

fn guid_eq(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

fn fmt_guid(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, g.data4[0], g.data4[1], g.data4[2], g.data4[3], g.data4[4],
        g.data4[5], g.data4[6], g.data4[7]
    )
}

fn new_guid() -> GUID {
    *NEW_GUID.get_or_init(|| guid_from_registry().unwrap_or(DEFAULT_NEW_GUID))
}

// ---------------------------------------------------------------------------------------------
// hooks
// ---------------------------------------------------------------------------------------------

fn is_programmer_path(p: *const u8) -> bool {
    if p.is_null() {
        return false;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(p as *const _) }.to_string_lossy().to_ascii_lowercase();
    s.contains("vid_04d8&pid_e11c")
}

unsafe extern "system" fn hook_setupdi_get_class_devs_a(
    guid: *const GUID, enumerator: *const u8, hwnd: *mut c_void, flags: u32,
) -> *mut c_void {
    let orig: SetupDiGetClassDevsAFn = std::mem::transmute(ORIG_SETUPDI.load(Ordering::Relaxed));
    if !guid.is_null() && guid_eq(&*guid, &OLD_GUID) {
        let g = new_guid();
        let r = orig(&g, enumerator, hwnd, flags);
        log!("SetupDiGetClassDevsA: old GUID -> {} (result {:?})", fmt_guid(&g), r);
        return r;
    }
    orig(guid, enumerator, hwnd, flags)
}

unsafe extern "system" fn hook_setupdi_enum_device_interfaces(
    set: *mut c_void, devinfo: *const c_void, guid: *const GUID, index: u32, data: *mut c_void,
) -> i32 {
    // The application passes the same (old) GUID here as to SetupDiGetClassDevsA.
    let orig: SetupDiEnumDeviceInterfacesFn = std::mem::transmute(ORIG_ENUMIF.load(Ordering::Relaxed));
    if !guid.is_null() && guid_eq(&*guid, &OLD_GUID) {
        let g = new_guid();
        return orig(set, devinfo, &g, index, data);
    }
    orig(set, devinfo, guid, index, data)
}

unsafe extern "system" fn hook_register_device_notification_a(
    recipient: *mut c_void, filter: *const c_void, flags: u32,
) -> *mut c_void {
    let orig: RegisterDeviceNotificationAFn = std::mem::transmute(ORIG_REGNOTIFY.load(Ordering::Relaxed));
    // DEV_BROADCAST_DEVICEINTERFACE_A { size, devicetype, reserved, classguid, name[] }
    if !filter.is_null() {
        let size = *(filter as *const u32) as usize;
        let devtype = *(filter as *const u32).add(1);
        if devtype == 5 && size >= 28 {
            let g = &*((filter as *const u8).add(12) as *const GUID);
            if guid_eq(g, &OLD_GUID) {
                let mut copy = std::slice::from_raw_parts(filter as *const u8, size).to_vec();
                let ng = new_guid();
                std::ptr::copy_nonoverlapping(&ng as *const GUID as *const u8, copy.as_mut_ptr().add(12), 16);
                let r = orig(recipient, copy.as_ptr() as *const c_void, flags);
                log!("RegisterDeviceNotificationA: old GUID -> {} (result {:?})", fmt_guid(&ng), r);
                return r;
            }
        }
    }
    orig(recipient, filter, flags)
}

unsafe fn find_pipes(iface: *mut c_void) -> (Option<u8>, Option<u8>) {
    // Same rule as the old driver's EvtDevicePrepareHardware: look at the first two pipes of the
    // interface and take the bulk IN and the bulk OUT pipe among them.
    let mut pin = None;
    let mut pout = None;
    for idx in 0..2u8 {
        let mut info: WINUSB_PIPE_INFORMATION = std::mem::zeroed();
        if WinUsb_QueryPipe(iface, 0, idx, &mut info) == 0 {
            log!("  QueryPipe({idx}) failed, err={}", windows_sys::Win32::Foundation::GetLastError());
            continue;
        }
        log!("  pipe[{idx}] type={} id=0x{:02x} maxpacket={}", info.PipeType, info.PipeId, info.MaximumPacketSize);
        if info.PipeType == 2 /* USBD_PipeTypeBulk */ {
            if info.PipeId & 0x80 != 0 {
                pin.get_or_insert(info.PipeId);
            } else {
                pout.get_or_insert(info.PipeId);
            }
        }
    }
    (pin, pout)
}

unsafe extern "system" fn hook_create_file_a(
    name: *const u8, access: u32, share: u32, sa: *const c_void, disp: u32, flags: u32, tmpl: *mut c_void,
) -> *mut c_void {
    let orig: CreateFileAFn = std::mem::transmute(ORIG_CREATE_FILE.load(Ordering::Relaxed));
    if !is_programmer_path(name) {
        return orig(name, access, share, sa, disp, flags, tmpl);
    }
    let path = std::ffi::CStr::from_ptr(name as *const _).to_string_lossy().into_owned();
    log!("CreateFileA(programmer) {path}");
    let h = orig(name, access, share, sa, disp, flags | FILE_FLAG_OVERLAPPED, tmpl);
    if h == INVALID_HANDLE_VALUE {
        log!("  open failed, err={}", windows_sys::Win32::Foundation::GetLastError());
        return h;
    }
    let mut iface: *mut c_void = std::ptr::null_mut();
    if WinUsb_Initialize(h, &mut iface) == 0 {
        let err = windows_sys::Win32::Foundation::GetLastError();
        log!("  WinUsb_Initialize failed, err={err} (is the device bound to WinUSB?)");
        CloseHandle(h);
        SetLastError(err);
        return INVALID_HANDLE_VALUE;
    }
    let (pin, pout) = find_pipes(iface);
    let (Some(pin), Some(pout)) = (pin, pout) else {
        log!("  no bulk IN/OUT pair found");
        WinUsb_Free(iface);
        CloseHandle(h);
        SetLastError(ERROR_INVALID_FUNCTION);
        return INVALID_HANDLE_VALUE;
    };
    let t = WRITE_TIMEOUT_MS;
    WinUsb_SetPipePolicy(iface, pout, PIPE_TRANSFER_TIMEOUT, 4, &t as *const u32 as *const c_void);
    DEVS.lock().unwrap().push(Dev { file: h as usize, iface: iface as usize, pipe_in: pin, pipe_out: pout });
    log!("  opened: handle={h:?} in=0x{pin:02x} out=0x{pout:02x}");
    h
}

unsafe extern "system" fn hook_close_handle(h: *mut c_void) -> i32 {
    let orig: CloseHandleFn = std::mem::transmute(ORIG_CLOSE.load(Ordering::Relaxed));
    let dev = {
        let mut g = DEVS.lock().unwrap();
        g.iter().position(|d| d.file == h as usize).map(|i| g.remove(i))
    };
    if let Some(d) = dev {
        WinUsb_Free(d.iface as *mut c_void);
        log!("CloseHandle(programmer) {h:?}");
    }
    orig(h)
}

unsafe extern "system" fn hook_device_io_control(
    h: *mut c_void, code: u32, inb: *const c_void, in_sz: u32, outb: *mut c_void, out_sz: u32,
    ret: *mut u32, ovl: *mut c_void,
) -> i32 {
    let dev = DEVS.lock().unwrap().iter().find(|d| d.file == h as usize).copied();
    let Some(d) = dev else {
        let orig: DeviceIoControlFn = std::mem::transmute(ORIG_IOCTL.load(Ordering::Relaxed));
        return orig(h, code, inb, in_sz, outb, out_sz, ret, ovl);
    };
    let iface = d.iface as *mut c_void;
    match code {
        IOCTL_WRITE => {
            if inb.is_null() || in_sz == 0 || in_sz > MAX_XFER {
                SetLastError(ERROR_INVALID_PARAMETER);
                return 0;
            }
            let mut n = 0u32;
            let ok = WinUsb_WritePipe(iface, d.pipe_out, inb as *const u8, in_sz, &mut n, std::ptr::null());
            let err = if ok == 0 { windows_sys::Win32::Foundation::GetLastError() } else { 0 };
            log!("WRITE {in_sz:>4} B -> {} (n={n}, err={err})  [{}]",
                 if ok != 0 { "ok" } else { "FAIL" }, hex(inb as *const u8, in_sz as usize));
            if !ret.is_null() {
                *ret = n;
            }
            if ok == 0 {
                SetLastError(err);
            } else {
                note_transfer(false, n);
            }
            (ok != 0) as i32
        }
        IOCTL_READ => {
            if inb.is_null() || in_sz < 4 || outb.is_null() || out_sz == 0 || out_sz > MAX_XFER {
                SetLastError(ERROR_INVALID_PARAMETER);
                return 0;
            }
            let mut timeout = *(inb as *const u32);
            if timeout == 0 {
                timeout = 1; // WinUSB treats 0 as "infinite"
            }
            WinUsb_SetPipePolicy(iface, d.pipe_in, PIPE_TRANSFER_TIMEOUT, 4, &timeout as *const u32 as *const c_void);
            let mut n = 0u32;
            let ok = WinUsb_ReadPipe(iface, d.pipe_in, outb as *mut u8, out_sz, &mut n, std::ptr::null());
            let err = if ok == 0 { windows_sys::Win32::Foundation::GetLastError() } else { 0 };
            log!("READ  {out_sz:>4} B (timeout {timeout} ms) -> {} (n={n}, err={err})  [{}]",
                 if ok != 0 { "ok" } else { "FAIL" }, hex(outb as *const u8, n as usize));
            if !ret.is_null() {
                *ret = n;
            }
            if ok == 0 {
                SetLastError(err);
            } else {
                note_transfer(true, n);
            }
            (ok != 0) as i32
        }
        _ => {
            log!("unsupported IOCTL 0x{code:08x} on programmer handle");
            SetLastError(ERROR_INVALID_FUNCTION);
            0
        }
    }
}

// ---------------------------------------------------------------------------------------------
// import-table patching of the main executable
// ---------------------------------------------------------------------------------------------

unsafe fn cstr_eq(p: *const u8, s: &[u8]) -> bool {
    for (i, &c) in s.iter().enumerate() {
        if *p.add(i) != c {
            return false;
        }
    }
    *p.add(s.len()) == 0
}

/// Address of the main module's IAT slot for the import named `func` (null if not imported by name).
unsafe fn find_iat_slot(func: &[u8]) -> *mut usize {
    let base = GetModuleHandleW(std::ptr::null()) as *const u8;
    let nt = base.add(*(base.add(0x3C) as *const u32) as usize);
    let imp_rva = *(nt.add(24 + 96 + 8) as *const u32) as usize; // IMAGE_DIRECTORY_ENTRY_IMPORT
    if imp_rva == 0 {
        return std::ptr::null_mut();
    }
    let mut desc = base.add(imp_rva);
    loop {
        let oft = *(desc as *const u32) as usize;
        let name_rva = *(desc.add(12) as *const u32) as usize;
        let first = *(desc.add(16) as *const u32) as usize;
        if name_rva == 0 && first == 0 {
            return std::ptr::null_mut();
        }
        let lookup = if oft != 0 { oft } else { first };
        let mut i = 0usize;
        loop {
            let thunk = *(base.add(lookup + i * 4) as *const u32);
            if thunk == 0 {
                break;
            }
            if thunk & 0x8000_0000 == 0 && cstr_eq(base.add(thunk as usize + 2), func) {
                return base.add(first + i * 4) as *mut usize;
            }
            i += 1;
        }
        desc = desc.add(20);
    }
}

unsafe fn install() {
    let _ = T0.set(Instant::now());
    open_log();
    log!("minipro_winusb shim loaded; WinUSB interface GUID = {}", fmt_guid(&new_guid()));
    let headless = headless::init();
    let mut hooks: Vec<(&[u8], usize, &AtomicUsize)> = vec![
        (b"SetupDiEnumDeviceInterfaces", hook_setupdi_enum_device_interfaces as *const () as usize, &ORIG_ENUMIF),
        (b"SetupDiGetClassDevsA", hook_setupdi_get_class_devs_a as *const () as usize, &ORIG_SETUPDI),
        (b"RegisterDeviceNotificationA", hook_register_device_notification_a as *const () as usize, &ORIG_REGNOTIFY),
        (b"CreateFileA", hook_create_file_a as *const () as usize, &ORIG_CREATE_FILE),
        (b"DeviceIoControl", hook_device_io_control as *const () as usize, &ORIG_IOCTL),
        (b"CloseHandle", hook_close_handle as *const () as usize, &ORIG_CLOSE),
    ];
    if headless {
        hooks.extend(headless::hooks());
    }
    let mut slots = vec![std::ptr::null_mut::<usize>(); hooks.len()];
    // Phase 1: locate every slot and remember the originals, so no hook can run without its trampoline.
    for (i, (name, _, orig)) in hooks.iter().enumerate() {
        let slot = find_iat_slot(name);
        if slot.is_null() {
            log!("import {} not found; shim inactive for it", String::from_utf8_lossy(name));
            continue;
        }
        orig.store(*slot, Ordering::SeqCst);
        slots[i] = slot;
    }
    // Phase 2: redirect.
    for (i, (name, hook, _)) in hooks.iter().enumerate() {
        let slot = slots[i];
        if slot.is_null() {
            continue;
        }
        let mut old_prot = 0u32;
        VirtualProtect(slot as *const c_void, 4, PAGE_READWRITE, &mut old_prot);
        *slot = *hook;
        VirtualProtect(slot as *const c_void, 4, old_prot, &mut old_prot);
        log!("hooked {}", String::from_utf8_lossy(name));
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(_h: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        install();
    }
    1
}
