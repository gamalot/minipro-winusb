//! "Headless engine" mode: the official MiniPro window is kept invisible and driven from outside.
//!
//! Enabled by the launcher's `--headless` switch (environment variable MINIPRO_WINUSB_HEADLESS=1).
//!
//!   * every top-level dialog is created and kept hidden (template WS_VISIBLE cleared, ShowWindow suppressed),
//!   * MessageBoxA is intercepted: reported as an event and answered with the safest button,
//!   * a named pipe `\\.\pipe\minipro_winusb_ctl` lets a front-end inspect and drive the hidden windows.
//!
//! Pipe protocol: one command per line, the reply is text lines ended by a line `<<END>>`.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{GetLastError, FALSE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeA, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetWindowThreadProcessId, GetClassNameA, GetDlgCtrlID, GetParent, GetWindowLongA, GetWindowRect,
    IsWindowVisible, PostMessageA, SendMessageTimeoutA, BM_CLICK, GWL_STYLE, SMTO_ABORTIFHUNG,
    WM_COMMAND, WM_GETTEXT, WS_CHILD, WS_EX_TOOLWINDOW, WS_VISIBLE,
};

use crate::{log, BYTES_IN, BYTES_OUT, LAST_XFER_MS, OPS_IN, OPS_OUT, T0};

pub static HEADLESS: AtomicBool = AtomicBool::new(false);
static UI_THREAD: AtomicU32 = AtomicU32::new(0);
static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

static ORIG_CREATE_DIALOG: AtomicUsize = AtomicUsize::new(0);
static ORIG_SHOW_WINDOW: AtomicUsize = AtomicUsize::new(0);
static ORIG_MESSAGE_BOX: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_OPEN: AtomicUsize = AtomicUsize::new(0);
static ORIG_GET_SAVE: AtomicUsize = AtomicUsize::new(0);
/// One-shot answers for the next file dialogs: (filter index to force (0 = leave), path)
static ARM_OPEN: Mutex<Option<(u32, String)>> = Mutex::new(None);
static ARM_SAVE: Mutex<Option<(u32, String)>> = Mutex::new(None);

const PIPE_NAME: &[u8] = b"\\\\.\\pipe\\minipro_winusb_ctl\0";

fn event(msg: String) {
    let ms = T0.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
    log(&format!("EVENT {msg}"));
    EVENTS.lock().unwrap().push(format!("{ms} {msg}"));
}

// ---------------------------------------------------------------------------------------------
// dialog template handling
// ---------------------------------------------------------------------------------------------

/// Length in bytes of a (non-extended) DLGTEMPLATE, or None for DLGTEMPLATEEX / on doubt.
unsafe fn template_len(p: *const u8) -> Option<usize> {
    let rd16 = |o: usize| u16::from_le_bytes([*p.add(o), *p.add(o + 1)]) as usize;
    let rd32 = |o: usize| u32::from_le_bytes([*p.add(o), *p.add(o + 1), *p.add(o + 2), *p.add(o + 3)]);
    if rd16(0) == 1 && rd16(2) == 0xFFFF {
        return None; // DLGTEMPLATEEX
    }
    let skip_sz = |mut o: usize| {
        while rd16(o) != 0 {
            o += 2;
        }
        o + 2
    };
    let skip_sz_or_ord = |o: usize| match rd16(o) {
        0 => o + 2,
        0xFFFF => o + 4,
        _ => skip_sz(o),
    };
    let style = rd32(0);
    let cdit = rd16(8);
    let mut o = 18;
    o = skip_sz_or_ord(o); // menu
    o = skip_sz_or_ord(o); // window class
    o = skip_sz(o); // title
    if style & 0x40 != 0 {
        // DS_SETFONT
        o += 2;
        o = skip_sz(o);
    }
    for _ in 0..cdit {
        o = (o + 3) & !3;
        o += 18;
        o = skip_sz_or_ord(o); // class
        o = skip_sz_or_ord(o); // title / resource
        let extra = rd16(o);
        o += 2 + extra;
        if o > 64 * 1024 {
            return None;
        }
    }
    Some(o)
}

type CreateDialogIndirectParamAFn =
    unsafe extern "system" fn(*mut c_void, *const u8, *mut c_void, *const c_void, isize) -> *mut c_void;

unsafe extern "system" fn hook_create_dialog(
    hinst: *mut c_void, tmpl: *const u8, parent: *mut c_void, dlgproc: *const c_void, param: isize,
) -> *mut c_void {
    let orig: CreateDialogIndirectParamAFn = std::mem::transmute(ORIG_CREATE_DIALOG.load(Ordering::Relaxed));
    let _ = UI_THREAD.compare_exchange(0, GetCurrentThreadId(), Ordering::SeqCst, Ordering::SeqCst);
    if !HEADLESS.load(Ordering::Relaxed) || tmpl.is_null() {
        return orig(hinst, tmpl, parent, dlgproc, param);
    }
    // DWORD-aligned private copy of the template with WS_VISIBLE cleared and WS_EX_TOOLWINDOW set
    let r = match template_len(tmpl) {
        Some(n) => {
            let mut buf = vec![0u32; n.div_ceil(4)];
            std::ptr::copy_nonoverlapping(tmpl, buf.as_mut_ptr() as *mut u8, n);
            buf[0] &= !WS_VISIBLE;
            buf[1] |= WS_EX_TOOLWINDOW;
            orig(hinst, buf.as_ptr() as *const u8, parent, dlgproc, param)
        }
        None => orig(hinst, tmpl, parent, dlgproc, param),
    };
    event(format!("dialog created hwnd={r:?} parent={parent:?} template_len={:?}", template_len(tmpl)));
    r
}

// ---------------------------------------------------------------------------------------------
// ShowWindow / MessageBox
// ---------------------------------------------------------------------------------------------

/// A window that is not a child control: the main frame, dialogs, owned popups.
unsafe fn is_toplevel(hwnd: *mut c_void) -> bool {
    !hwnd.is_null() && (GetWindowLongA(hwnd, GWL_STYLE) as u32 & WS_CHILD) == 0
}

type ShowWindowFn = unsafe extern "system" fn(*mut c_void, i32) -> i32;

unsafe extern "system" fn hook_show_window(hwnd: *mut c_void, cmd: i32) -> i32 {
    let orig: ShowWindowFn = std::mem::transmute(ORIG_SHOW_WINDOW.load(Ordering::Relaxed));
    if HEADLESS.load(Ordering::Relaxed) && cmd != 0 && is_toplevel(hwnd) {
        event(format!("ShowWindow({hwnd:?}, {cmd}) suppressed"));
        return orig(hwnd, 0);
    }
    orig(hwnd, cmd)
}

type MessageBoxAFn = unsafe extern "system" fn(*mut c_void, *const u8, *const u8, u32) -> i32;

unsafe fn cstr(p: *const u8) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p as *const _).to_string_lossy().into_owned()
    }
}

unsafe extern "system" fn hook_message_box(hwnd: *mut c_void, text: *const u8, caption: *const u8, flags: u32) -> i32 {
    if !HEADLESS.load(Ordering::Relaxed) {
        let orig: MessageBoxAFn = std::mem::transmute(ORIG_MESSAGE_BOX.load(Ordering::Relaxed));
        return orig(hwnd, text, caption, flags);
    }
    // safest answer for each button set (IDOK=1 IDCANCEL=2 IDABORT=3 IDRETRY=4 IDIGNORE=5 IDYES=6 IDNO=7)
    let answer = match flags & 0xF {
        0 => 1,       // MB_OK
        1 => 2,       // MB_OKCANCEL -> cancel
        2 => 3,       // MB_ABORTRETRYIGNORE -> abort
        3 => 2,       // MB_YESNOCANCEL -> cancel
        4 => 7,       // MB_YESNO -> no
        5 => 2,       // MB_RETRYCANCEL -> cancel
        _ => 2,
    };
    event(format!(
        "MessageBox flags=0x{flags:x} caption={:?} text={:?} -> answered {answer}",
        cstr(caption),
        cstr(text)
    ));
    answer
}


// ---------------------------------------------------------------------------------------------
// GetOpenFileNameA / GetSaveFileNameA
// ---------------------------------------------------------------------------------------------

#[repr(C)]
struct OpenFileNameA {
    l_struct_size: u32,
    hwnd_owner: *mut c_void,
    h_instance: *mut c_void,
    lpstr_filter: *const u8,
    lpstr_custom_filter: *mut u8,
    n_max_cust_filter: u32,
    n_filter_index: u32,
    lpstr_file: *mut u8,
    n_max_file: u32,
    lpstr_file_title: *mut u8,
    n_max_file_title: u32,
    lpstr_initial_dir: *const u8,
    lpstr_title: *const u8,
    flags: u32,
    n_file_offset: u16,
    n_file_extension: u16,
    lpstr_def_ext: *const u8,
}

/// "Desc\0*.pat\0Desc2\0*.pat2\0\0" -> "Desc|*.pat ; Desc2|*.pat2"
unsafe fn multistr(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut parts = Vec::new();
    let mut q = p;
    loop {
        let s = cstr(q);
        if s.is_empty() || parts.len() > 64 {
            break;
        }
        q = q.add(s.len() + 1);
        parts.push(s);
    }
    parts.chunks(2).map(|c| c.join("|")).collect::<Vec<_>>().join(" ; ")
}

type GetFileNameAFn = unsafe extern "system" fn(*mut OpenFileNameA) -> i32;

unsafe fn file_dialog(save: bool, ofn: *mut OpenFileNameA) -> i32 {
    let o = &mut *ofn;
    let kind = if save { "SaveFile" } else { "OpenFile" };
    let armed = if save { ARM_SAVE.lock().unwrap().take() } else { ARM_OPEN.lock().unwrap().take() };
    event(format!(
        "{kind} dialog title={:?} filter=[{}] filterindex={} defext={:?} initdir={:?} flags=0x{:x} file={:?} -> {}",
        cstr(o.lpstr_title),
        multistr(o.lpstr_filter),
        o.n_filter_index,
        cstr(o.lpstr_def_ext),
        cstr(o.lpstr_initial_dir),
        o.flags,
        cstr(o.lpstr_file),
        match &armed {
            Some((_, p)) => format!("answered with {p:?}"),
            None => "cancelled (not armed)".to_string(),
        }
    ));
    let Some((idx, path)) = armed else {
        return 0; // cancel: never show a system file dialog while headless
    };
    let bytes = path.as_bytes();
    if o.lpstr_file.is_null() || bytes.len() + 2 > o.n_max_file as usize {
        return 0;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), o.lpstr_file, bytes.len());
    *o.lpstr_file.add(bytes.len()) = 0;
    *o.lpstr_file.add(bytes.len() + 1) = 0; // double NUL for multi-select style buffers
    o.n_file_offset = path.rfind('\\').map(|i| i + 1).unwrap_or(0) as u16;
    o.n_file_extension = path.rfind('.').map(|i| i + 1).unwrap_or(0) as u16;
    if idx != 0 {
        o.n_filter_index = idx;
    }
    1
}

unsafe extern "system" fn hook_get_open_file_name(ofn: *mut OpenFileNameA) -> i32 {
    if !HEADLESS.load(Ordering::Relaxed) {
        let orig: GetFileNameAFn = std::mem::transmute(ORIG_GET_OPEN.load(Ordering::Relaxed));
        return orig(ofn);
    }
    file_dialog(false, ofn)
}

unsafe extern "system" fn hook_get_save_file_name(ofn: *mut OpenFileNameA) -> i32 {
    if !HEADLESS.load(Ordering::Relaxed) {
        let orig: GetFileNameAFn = std::mem::transmute(ORIG_GET_SAVE.load(Ordering::Relaxed));
        return orig(ofn);
    }
    file_dialog(true, ofn)
}

pub fn hooks() -> Vec<(&'static [u8], usize, &'static AtomicUsize)> {
    vec![
        (b"GetOpenFileNameA", hook_get_open_file_name as *const () as usize, &ORIG_GET_OPEN),
        (b"GetSaveFileNameA", hook_get_save_file_name as *const () as usize, &ORIG_GET_SAVE),
        (b"CreateDialogIndirectParamA", hook_create_dialog as *const () as usize, &ORIG_CREATE_DIALOG),
        (b"ShowWindow", hook_show_window as *const () as usize, &ORIG_SHOW_WINDOW),
        (b"MessageBoxA", hook_message_box as *const () as usize, &ORIG_MESSAGE_BOX),
    ]
}

// ---------------------------------------------------------------------------------------------
// window inspection helpers
// ---------------------------------------------------------------------------------------------

unsafe fn window_text(hwnd: *mut c_void) -> String {
    let mut buf = [0u8; 512];
    let mut res = 0usize;
    let ok = SendMessageTimeoutA(hwnd, WM_GETTEXT, buf.len(), buf.as_mut_ptr() as isize, SMTO_ABORTIFHUNG, 500, &mut res);
    if ok == 0 {
        return "<timeout>".into();
    }
    String::from_utf8_lossy(&buf[..(res as usize).min(buf.len())]).trim_end_matches('\0').replace('\n', "\\n")
}

unsafe fn describe(hwnd: *mut c_void) -> String {
    let mut cls = [0u8; 64];
    let n = GetClassNameA(hwnd, cls.as_mut_ptr(), cls.len() as i32) as usize;
    let mut r = std::mem::zeroed();
    GetWindowRect(hwnd, &mut r);
    format!(
        "{hwnd:?} id={} class={} vis={} en={} owner={:?} rect=({},{} {}x{}) text={:?}",
        GetDlgCtrlID(hwnd),
        String::from_utf8_lossy(&cls[..n]),
        (IsWindowVisible(hwnd) != 0) as u8,
        (IsWindowEnabled(hwnd) != 0) as u8,
        GetParent(hwnd),
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        window_text(hwnd)
    )
}

unsafe extern "system" fn enum_collect(hwnd: *mut c_void, lparam: isize) -> i32 {
    let v = &mut *(lparam as *mut Vec<String>);
    v.push(describe(hwnd));
    TRUE
}

unsafe extern "system" fn enum_process_windows(hwnd: *mut c_void, lparam: isize) -> i32 {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == GetCurrentProcessId() {
        let v = &mut *(lparam as *mut Vec<String>);
        v.push(describe(hwnd));
    }
    TRUE
}

fn num(s: &str) -> Option<usize> {
    let s = s.trim().trim_start_matches('#');
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        usize::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

// ---------------------------------------------------------------------------------------------
// control pipe
// ---------------------------------------------------------------------------------------------

unsafe fn handle(cmd: &str) -> String {
    let mut it = cmd.split_whitespace();
    let name = it.next().unwrap_or("");
    let args: Vec<usize> = it.filter_map(num).collect();
    let hw = |i: usize| args.get(i).copied().unwrap_or(0) as *mut c_void;
    match name {
        "ping" => "pong".into(),
        "windows" => {
            // all top-level windows of this process (operations run on worker threads and create their own dialogs)
            let mut v: Vec<String> = Vec::new();
            EnumWindows(Some(enum_process_windows), &mut v as *mut _ as isize);
            v.join("\n")
        }
        "tree" => {
            let mut v: Vec<String> = Vec::new();
            EnumChildWindows(hw(0), Some(enum_collect), &mut v as *mut _ as isize);
            v.join("\n")
        }
        "info" => describe(hw(0)),
        "text" => window_text(hw(0)),
        "cmd" => {
            // WM_COMMAND id from a menu / accelerator
            let ok = PostMessageA(hw(0), WM_COMMAND, args.get(1).copied().unwrap_or(0), 0);
            format!("posted WM_COMMAND {} -> {ok}", args.get(1).copied().unwrap_or(0))
        }
        "click" => {
            let ok = PostMessageA(hw(0), BM_CLICK, 0, 0);
            format!("posted BM_CLICK -> {ok}")
        }
        "post" => {
            let ok = PostMessageA(hw(0), args.get(1).copied().unwrap_or(0) as u32, args.get(2).copied().unwrap_or(0), args.get(3).copied().unwrap_or(0) as isize);
            format!("posted -> {ok}")
        }
        "send" => {
            let mut res = 0usize;
            let ok = SendMessageTimeoutA(
                hw(0),
                args.get(1).copied().unwrap_or(0) as u32,
                args.get(2).copied().unwrap_or(0),
                args.get(3).copied().unwrap_or(0) as isize,
                SMTO_ABORTIFHUNG,
                3000,
                &mut res,
            );
            format!("sent -> ok={ok} result={res} (0x{res:x})")
        }
        "lb" => {
            // list box lines: `lb <hwnd> [start] [count]`; first reply line is `count=<total>`
            let h = hw(0);
            let start = args.get(1).copied().unwrap_or(0);
            let want = args.get(2).copied().unwrap_or(100_000);
            let mut total = 0usize;
            if SendMessageTimeoutA(h, 0x018B, 0, 0, SMTO_ABORTIFHUNG, 1000, &mut total) == 0 {
                return "<timeout>".into();
            }
            let mut lines = vec![format!("count={total}")];
            for i in start..total.min(start.saturating_add(want)) {
                let mut len = 0usize;
                SendMessageTimeoutA(h, 0x018A, i, 0, SMTO_ABORTIFHUNG, 1000, &mut len);
                let mut buf = vec![0u8; len.min(4096) + 2];
                let mut n = 0usize;
                SendMessageTimeoutA(h, 0x0189, i, buf.as_mut_ptr() as isize, SMTO_ABORTIFHUNG, 1000, &mut n);
                lines.push(String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned());
            }
            lines.join("\n")
        }
        "lbpick" => {
            // select item <index> of a list box and notify the parent dialog (LBN_SELCHANGE), like a mouse click
            let h = hw(0);
            let idx = args.get(1).copied().unwrap_or(0);
            let mut res = 0usize;
            SendMessageTimeoutA(h, 0x0186, idx, 0, SMTO_ABORTIFHUNG, 1000, &mut res); // LB_SETCURSEL
            let parent = GetParent(h);
            let id = GetDlgCtrlID(h) as usize;
            let ok = PostMessageA(parent, WM_COMMAND, (1usize << 16) | id, h as isize); // LBN_SELCHANGE = 1
            format!("lbpick -> setcursel={res} notify={ok}")
        }
        "sbparts" => {
            // status bar parts: index<TAB>text
            let h = hw(0);
            let mut count = 0usize;
            SendMessageTimeoutA(h, 0x0406, 0, 0, SMTO_ABORTIFHUNG, 1000, &mut count); // SB_GETPARTS
            let mut out = Vec::new();
            for i in 0..count.min(16) {
                let mut buf = [0u8; 256];
                let mut r = 0usize;
                SendMessageTimeoutA(h, 0x0402, i, buf.as_mut_ptr() as isize, SMTO_ABORTIFHUNG, 1000, &mut r); // SB_GETTEXTA
                let n = r & 0xFFFF;
                out.push(format!("{i}\t{}", String::from_utf8_lossy(&buf[..n.min(255)])));
            }
            out.join("\n")
        }
        "check" => {
            let mut res = 0usize;
            SendMessageTimeoutA(hw(0), 0x00F0, 0, 0, SMTO_ABORTIFHUNG, 1000, &mut res); // BM_GETCHECK
            res.to_string()
        }
        "settext" => {
            let text = cmd.splitn(3, ' ').nth(2).unwrap_or("");
            let c = std::ffi::CString::new(text).unwrap_or_default();
            let mut res = 0usize;
            let ok = SendMessageTimeoutA(hw(0), 0x000C, 0, c.as_ptr() as isize, SMTO_ABORTIFHUNG, 1000, &mut res); // WM_SETTEXT
            format!("settext -> ok={ok}")
        }
        "arm-open" | "arm-save" => {
            // arm-save <filter index, 0 = leave> <path with spaces allowed>
            let rest = cmd.splitn(2, ' ').nth(1).unwrap_or("").trim();
            let (idx, path) = match rest.split_once(' ') {
                Some((a, b)) if a.parse::<u32>().is_ok() => (a.parse::<u32>().unwrap(), b.trim()),
                _ => (0, rest),
            };
            let slot = if name == "arm-save" { &ARM_SAVE } else { &ARM_OPEN };
            *slot.lock().unwrap() = Some((idx, path.to_string()));
            format!("armed {name} filter={idx} path={path:?}")
        }
        "disarm" => {
            *ARM_OPEN.lock().unwrap() = None;
            *ARM_SAVE.lock().unwrap() = None;
            "disarmed".into()
        }
        "stats" => {
            let now = T0.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
            let last = LAST_XFER_MS.load(Ordering::Relaxed);
            format!(
                "in_bytes={} out_bytes={} in_ops={} out_ops={} idle_ms={}",
                BYTES_IN.load(Ordering::Relaxed),
                BYTES_OUT.load(Ordering::Relaxed),
                OPS_IN.load(Ordering::Relaxed),
                OPS_OUT.load(Ordering::Relaxed),
                now.saturating_sub(last)
            )
        }
        "stats-reset" => {
            for c in [&BYTES_IN, &BYTES_OUT, &OPS_IN, &OPS_OUT] {
                c.store(0, Ordering::Relaxed);
            }
            "reset".into()
        }
        "capture" => {
            // capture <dialog hwnd> <control hwnd or 0> <path>: what the (hidden) dialog paints, cropped to the control
            let path = cmd.splitn(4, ' ').nth(3).unwrap_or("").trim();
            capture(hw(0), hw(1), path)
        }
        "events" => {
            let mut g = EVENTS.lock().unwrap();
            let s = g.join("\n");
            g.clear();
            s
        }
        "" => String::new(),
        _ => format!("error: unknown command {name:?}"),
    }
}

#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: *mut c_void, hdc: *mut c_void, flags: u32) -> i32;
}

/// Renders a window of this process (it may be hidden) into a bitmap with PrintWindow and writes the part
/// covered by `ctrl` (or everything) to `path` as: u32 width, u32 height (little endian), then BGRA rows, top-down.
/// Only the official program's own window is drawn; nothing is read from the screen.
unsafe fn capture(dlg: *mut c_void, ctrl: *mut c_void, path: &str) -> String {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject, BITMAPINFO, DIB_RGB_COLORS,
    };
    use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, RDW_ALLCHILDREN, RDW_ERASE, RDW_INVALIDATE, RDW_UPDATENOW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER};
    // Windows renders nothing for a window that is not shown, so a hidden dialog is parked far off-screen,
    // shown there without activation for the moment of the capture, and hidden again.  (These are direct user32
    // calls: the ShowWindow hook only covers MiniPro.exe's own import table.)
    let hidden = IsWindowVisible(dlg) == 0;
    let mut home: RECT = std::mem::zeroed();
    GetWindowRect(dlg, &mut home);
    if hidden {
        SetWindowPos(dlg, std::ptr::null_mut(), -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        ShowWindow(dlg, 4); // SW_SHOWNOACTIVATE
        RedrawWindow(dlg, std::ptr::null(), std::ptr::null_mut(), RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW);
    }
    let restore = |dlg: *mut c_void| {
        if hidden {
            ShowWindow(dlg, 0); // SW_HIDE
            SetWindowPos(dlg, std::ptr::null_mut(), home.left, home.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    };
    let mut wr: RECT = std::mem::zeroed();
    GetWindowRect(dlg, &mut wr);
    let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
    if w <= 0 || h <= 0 || w > 8000 || h > 8000 {
        restore(dlg);
        return format!("error: window size {w}x{h}");
    }
    let screen = GetDC(std::ptr::null_mut());
    let dc = CreateCompatibleDC(screen);
    ReleaseDC(std::ptr::null_mut(), screen);
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = std::mem::size_of_val(&bi.bmiHeader) as u32;
    bi.bmiHeader.biWidth = w;
    bi.bmiHeader.biHeight = -h; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
    if bmp.is_null() || bits.is_null() {
        DeleteDC(dc);
        restore(dlg);
        return "error: CreateDIBSection failed".into();
    }
    let old = SelectObject(dc, bmp);
    let ok = PrintWindow(dlg, dc, 0);
    GdiFlush();
    let all = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
    let (mut cx, mut cy, mut cw, mut ch) = (0, 0, w, h);
    if !ctrl.is_null() {
        let mut cr: RECT = std::mem::zeroed();
        GetWindowRect(ctrl, &mut cr);
        cx = (cr.left - wr.left).clamp(0, w);
        cy = (cr.top - wr.top).clamp(0, h);
        cw = (cr.right - cr.left).clamp(0, w - cx);
        ch = (cr.bottom - cr.top).clamp(0, h - cy);
    }
    let mut out = Vec::with_capacity(8 + (cw * ch * 4) as usize);
    out.extend_from_slice(&(cw as u32).to_le_bytes());
    out.extend_from_slice(&(ch as u32).to_le_bytes());
    for y in cy..cy + ch {
        let a = ((y * w + cx) * 4) as usize;
        out.extend_from_slice(&all[a..a + (cw * 4) as usize]);
    }
    SelectObject(dc, old);
    DeleteObject(bmp);
    DeleteDC(dc);
    restore(dlg);
    match std::fs::write(path, out) {
        Ok(()) => format!("ok {cw} {ch} printwindow={ok}"),
        Err(e) => format!("error: write {path:?}: {e}"),
    }
}

fn pipe_server() {
    unsafe {
        loop {
            let h = CreateNamedPipeA(
                PIPE_NAME.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                65536,
                65536,
                0,
                std::ptr::null(),
            );
            if h == INVALID_HANDLE_VALUE {
                log(&format!("CreateNamedPipe failed, err={}", GetLastError()));
                return;
            }
            if ConnectNamedPipe(h, std::ptr::null_mut()) == FALSE && GetLastError() != 535 {
                // 535 = ERROR_PIPE_CONNECTED (client connected before ConnectNamedPipe)
                windows_sys::Win32::Foundation::CloseHandle(h);
                continue;
            }
            let mut pending: Vec<u8> = Vec::new();
            'session: loop {
                let mut buf = [0u8; 4096];
                let mut n = 0u32;
                if ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &mut n, std::ptr::null_mut()) == 0 || n == 0 {
                    break;
                }
                pending.extend_from_slice(&buf[..n as usize]);
                while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=pos).collect();
                    let cmd = String::from_utf8_lossy(&line).trim().to_string();
                    let mut reply = handle(&cmd);
                    reply.push_str("\n<<END>>\n");
                    let mut w = 0u32;
                    if WriteFile(h, reply.as_ptr(), reply.len() as u32, &mut w, std::ptr::null_mut()) == 0 {
                        break 'session;
                    }
                }
            }
            DisconnectNamedPipe(h);
            windows_sys::Win32::Foundation::CloseHandle(h);
        }
    }
}

/// Called once at start-up (from DllMain): switch to headless mode if requested.
pub fn init() -> bool {
    if std::env::var_os("MINIPRO_WINUSB_HEADLESS").is_none() {
        return false;
    }
    HEADLESS.store(true, Ordering::SeqCst);
    log("headless mode on");
    std::thread::spawn(pipe_server);
    true
}
