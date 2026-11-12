//! Client for the headless official MiniPro program (see ../../shim/src/headless.rs) and the worker thread
//! that turns user actions into the official program's own dialog flows.

use crate::lang::{tr, trf};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;

const PIPE: &str = r"\\.\pipe\minipro_winusb_ctl";
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;

// ---------------------------------------------------------------------------------------------
// public types
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct ChipEntry {
    pub name: String, // e.g. "SST39SF020A @PLCC32"
    pub mfr: String,
}

#[derive(Clone, Debug, Default)]
pub struct ChipState {
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub size_text: String,
    pub checksum: String,
    pub chip_id: String,
    pub erase_before: Option<bool>,
    pub verify_after: Option<bool>,
    pub blank_before: Option<bool>,
    pub check_id: Option<bool>,
    pub skip_ff: Option<bool>,
}

fn no_device() -> &'static str {
    tr("找不到编程器，请检查 USB 连接", "Programmer not found, check the USB connection")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Program,
    Verify,
    Erase,
    Blank,
}

impl Op {
    fn menu(self) -> usize {
        match self {
            Op::Read => 1002,
            Op::Program => 1003,
            Op::Verify => 1005,
            Op::Erase => 32796,
            Op::Blank => 32801,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Op::Read => tr("读取", "Read"),
            Op::Program => tr("写入", "Program"),
            Op::Verify => tr("校验", "Verify"),
            Op::Erase => tr("擦除", "Erase"),
            Op::Blank => tr("查空", "Blank check"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionId {
    EraseBefore,
    VerifyAfter,
    BlankBefore,
    CheckId,
    SkipFf,
}

impl OptionId {
    fn control(self) -> i32 {
        match self {
            OptionId::EraseBefore => 1031,
            OptionId::VerifyAfter => 1032,
            OptionId::BlankBefore => 1040,
            OptionId::CheckId => 1045,
            OptionId::SkipFf => 1035,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
    Error,
}

pub enum Job {
    Refresh,
    ListChips,
    SelectChip(ChipEntry),
    Identify,
    /// read device code / serial number from the official About dialog
    ReadInfo,
    /// fetch the official "location in socket" picture for the selected chip
    SocketImage,
    /// close and re-launch the hidden official program (it never re-opens a device that was re-plugged)
    Restart,
    SelfCheck,
    /// run an operation; for program / verify the data shown in the UI comes along and is handed to the engine
    /// (and checked) right before, so the chip always gets exactly what is on screen
    Run(Op, Option<Vec<u8>>),
    FetchBuffer,
    SetOption(OptionId, bool),
    Quit,
}

pub enum Event {
    Log(Level, String),
    /// `frac` None = indeterminate
    Progress { frac: Option<f32>, text: String },
    State(ChipState),
    Buffer(Vec<u8>),
    ChipList(Vec<ChipEntry>),
    /// (official software line, device code, serial number)
    Programmer(String, String, String),
    /// the official "location in socket" picture: width, height, RGBA pixels (None = not available)
    Socket(Option<(usize, usize, Vec<u8>)>),
    /// the engine was re-launched: its buffer is empty again
    Restarted,
    OpDone { op: &'static str, ok: bool, summary: String },
    Ready,
    Busy(bool),
    Fatal(String),
    /// the engine has been shut down after Job::Quit
    Quitted,
}

// ---------------------------------------------------------------------------------------------
// pipe + windows
// ---------------------------------------------------------------------------------------------

struct Pipe {
    w: File,
    r: BufReader<File>,
}

impl Pipe {
    fn connect(timeout: Duration) -> Option<Pipe> {
        let t0 = Instant::now();
        loop {
            if let Ok(f) = OpenOptions::new().read(true).write(true).open(PIPE) {
                if let Ok(c) = f.try_clone() {
                    return Some(Pipe { w: f, r: BufReader::new(c) });
                }
            }
            if t0.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn call(&mut self, cmd: &str) -> std::io::Result<Vec<String>> {
        self.w.write_all(cmd.as_bytes())?;
        self.w.write_all(b"\n")?;
        self.w.flush()?;
        let mut out = Vec::new();
        loop {
            let mut line = String::new();
            if self.r.read_line(&mut line)? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            let l = line.trim_end_matches(['\r', '\n']);
            if l == "<<END>>" {
                break;
            }
            out.push(l.to_string());
        }
        if out.len() == 1 && out[0].is_empty() {
            out.clear();
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default)]
struct Win {
    hwnd: String,
    id: i32,
    class: String,
    en: bool,
    text: String,
}

fn parse_win(line: &str) -> Option<Win> {
    let line = line.trim();
    let hwnd = line.split(' ').next()?.to_string();
    if !hwnd.starts_with("0x") {
        return None;
    }
    let field = |key: &str| -> Option<&str> {
        let i = line.find(key)? + key.len();
        Some(line[i..].split(' ').next().unwrap_or(""))
    };
    let text = line.find(" text=\"").map(|i| line[i + 7..].trim_end_matches('"').to_string()).unwrap_or_default();
    Some(Win {
        hwnd,
        id: field(" id=")?.parse().ok()?,
        class: field(" class=")?.to_string(),
        en: field(" en=") == Some("1"),
        text,
    })
}

// ---------------------------------------------------------------------------------------------
// engine
// ---------------------------------------------------------------------------------------------

pub struct Engine {
    pipe: Pipe,
    frame: String,
    form: HashMap<i32, String>,
    tx: Sender<Event>,
    ctx: egui::Context,
    /// the control pipe failed: the engine crashed or exited; every further call is skipped
    broken: bool,
    /// process id of the hidden MiniPro.exe, to make sure it is really gone on quit / restart
    pid: u32,
    /// the official program was set to Chinese and was switched to English for this session
    zh_restore: bool,
}

fn engine_lost() -> &'static str {
    tr("编程引擎没有响应（可能已经退出）", "The programming engine stopped responding (it may have exited)")
}

/// Waits up to `grace_ms` for process `pid` to exit, then terminates it.
fn end_process(pid: u32, grace_ms: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
    if pid == 0 {
        return;
    }
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid);
        if h.is_null() {
            return; // already gone
        }
        if WaitForSingleObject(h, grace_ms) != 0 {
            TerminateProcess(h, 1);
            WaitForSingleObject(h, 3000);
        }
        CloseHandle(h);
    }
}

/// A file in the temp directory for exchanging the buffer with the official program.  MiniPro is an ANSI
/// program, so a temp directory with non-ASCII characters (e.g. a Chinese user name on a Western system) is
/// replaced by its 8.3 short name when there is one.  Otherwise the path is used as it is and the shim refuses
/// it if the ANSI code page cannot express it - the operation then fails cleanly instead of using stale data.
fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir();
    if dir.to_string_lossy().is_ascii() {
        return dir.join(name);
    }
    short_path(&dir).filter(|p| p.to_string_lossy().is_ascii()).unwrap_or(dir).join(name)
}

fn short_path(p: &std::path::Path) -> Option<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])))
}

impl Engine {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
    fn log(&self, level: Level, msg: impl Into<String>) {
        self.send(Event::Log(level, msg.into()));
    }
    fn progress(&self, frac: Option<f32>, text: impl Into<String>) {
        self.send(Event::Progress { frac, text: text.into() });
    }

    fn call(&mut self, cmd: &str) -> Vec<String> {
        if self.broken {
            return Vec::new();
        }
        match self.pipe.call(cmd) {
            Ok(v) => v,
            Err(_) => {
                self.broken = true;
                Vec::new()
            }
        }
    }

    /// Restores the official program's language if it was switched, closes it, and makes sure the process is
    /// gone (terminated after `grace_ms`).
    fn shutdown(&mut self, grace_ms: u32) {
        let frame = self.frame.clone();
        if self.zh_restore {
            self.post(&frame, WM_COMMAND, 32834);
            std::thread::sleep(Duration::from_millis(300));
        }
        self.post(&frame, WM_CLOSE, 0);
        end_process(self.pid, grace_ms);
    }

    fn windows(&mut self) -> Vec<Win> {
        self.call("windows").iter().filter_map(|l| parse_win(l)).collect()
    }
    fn tree(&mut self, h: &str) -> Vec<Win> {
        self.call(&format!("tree {h}")).iter().filter_map(|l| parse_win(l)).collect()
    }
    fn text(&mut self, h: &str) -> String {
        self.call(&format!("text {h}")).join("\n")
    }
    fn child(&mut self, parent: &str, id: i32) -> Option<Win> {
        self.tree(parent).into_iter().find(|w| w.id == id)
    }
    fn is_enabled(&mut self, h: &str) -> bool {
        self.call(&format!("info {h}")).first().map(|l| l.contains(" en=1")).unwrap_or(false)
    }
    fn post(&mut self, h: &str, msg: u32, wp: usize) {
        self.call(&format!("post {h} {msg} {wp} 0"));
    }
    fn click(&mut self, h: &str) {
        self.call(&format!("click {h}"));
    }
    fn lb(&mut self, h: &str) -> Vec<String> {
        let mut v = self.call(&format!("lb {h}"));
        if !v.is_empty() {
            v.remove(0); // "count=N"
        }
        v.into_iter().map(|s| s.trim().to_string()).collect()
    }

    fn dialogs(&mut self) -> Vec<Win> {
        self.windows().into_iter().filter(|w| w.class == "#32770").collect()
    }

    /// Wait for a top-level dialog that is not in `known` and that contains control `must_have`.
    fn wait_dialog(&mut self, known: &[String], must_have: i32, timeout: Duration) -> Option<(Win, Vec<Win>)> {
        let t0 = Instant::now();
        while t0.elapsed() < timeout && !self.broken {
            for d in self.dialogs() {
                if known.contains(&d.hwnd) {
                    continue;
                }
                let t = self.tree(&d.hwnd);
                if t.iter().any(|w| w.id == must_have) {
                    return Some((d, t));
                }
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        None
    }

    fn known_dialogs(&mut self) -> Vec<String> {
        self.dialogs().into_iter().map(|d| d.hwnd).collect()
    }

    fn close_dialog(&mut self, hwnd: &str) {
        self.post(hwnd, WM_COMMAND, 2); // IDCANCEL
        std::thread::sleep(Duration::from_millis(250));
    }

    // ---- start-up -------------------------------------------------------------------------

    fn start(tx: Sender<Event>, ctx: egui::Context) -> Result<Engine, String> {
        let mut pipe = Pipe::connect(Duration::from_millis(300));
        if pipe.is_none() {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let launcher = exe.with_file_name("minipro-launch.exe");
            if !launcher.is_file() {
                return Err(trf!("找不到启动器 {}", "Launcher not found: {}", launcher.display()));
            }
            std::process::Command::new(&launcher)
                .arg("--headless")
                .spawn()
                .map_err(|e| trf!("无法启动官方程序: {e}", "Cannot start the official program: {e}"))?;
            pipe = Pipe::connect(Duration::from_secs(25));
        }
        let pipe = pipe.ok_or(tr("无法连接到官方程序的控制通道", "Cannot connect to the official program's control channel"))?;
        let mut e = Engine { pipe, frame: String::new(), form: HashMap::new(), tx, ctx, broken: false, pid: 0, zh_restore: false };
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(25) {
            if let Some(w) = e.windows().into_iter().find(|w| w.class.starts_with("Afx:") && w.text.contains("MiniPro")) {
                e.frame = w.hwnd;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if e.frame.is_empty() {
            return Err(tr("官方程序主窗口没有出现", "The official program's main window did not appear").into());
        }
        std::thread::sleep(Duration::from_millis(600));
        e.pid = e.call("pid").first().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        // Results are recognised by the official program's English texts: if it is set to Chinese, switch it to
        // English for this session (it is switched back on exit).
        let frame = e.frame.clone();
        // (menu item "Read" in English, non-ASCII in Chinese)
        let read_item = e.call(&format!("menutext {frame} 1002")).join("");
        if !read_item.starts_with("error") && !read_item.trim().is_empty() && !read_item.is_ascii() {
            e.post(&frame, WM_COMMAND, 32835);
            std::thread::sleep(Duration::from_millis(800));
            e.zh_restore = true;
        }
        e.cache_form();
        e.dismiss_startup_dialogs();
        if e.broken {
            return Err(engine_lost().into());
        }
        Ok(e)
    }

    fn cache_form(&mut self) {
        let frame = self.frame.clone();
        for w in self.tree(&frame) {
            self.form.entry(w.id).or_insert(w.hwnd);
        }
    }

    /// Close a "Select Device" dialog that the official program opened by itself (it sometimes pops up a few
    /// seconds after start-up and would swallow the next operation).
    fn dismiss_stray_dialogs(&mut self) -> bool {
        let mut closed = false;
        for d in self.dialogs() {
            let t = self.tree(&d.hwnd);
            if t.iter().any(|w| w.id == 1219) {
                self.close_dialog(&d.hwnd);
                closed = true;
            }
        }
        closed
    }

    fn dismiss_startup_dialogs(&mut self) {
        // watch for a while: the dialog has been seen ~3 s after the window appeared
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(4500) {
            self.dismiss_stray_dialogs();
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn form_text(&mut self, id: i32) -> String {
        match self.form.get(&id).cloned() {
            Some(h) => self.text(&h),
            None => String::new(),
        }
    }
    fn form_check(&mut self, id: i32) -> Option<bool> {
        let h = self.form.get(&id)?.clone();
        if !self.is_enabled(&h) {
            return None;
        }
        Some(self.call(&format!("check {h}")).first().map(|s| s.trim() == "1").unwrap_or(false))
    }

    pub fn read_state(&mut self) -> ChipState {
        let size_text = self.form_text(1008);
        let size = size_text
            .split_whitespace()
            .next()
            .and_then(|s| s.strip_prefix("0x"))
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0);
        ChipState {
            name: self.form_text(1030),
            kind: self.form_text(1125),
            size,
            size_text,
            checksum: self.form_text(1124),
            chip_id: {
                let t = self.form_text(1043).trim().to_string();
                if t == "Static" { String::new() } else { t }
            },
            erase_before: self.form_check(1031),
            verify_after: self.form_check(1032),
            blank_before: self.form_check(1040),
            check_id: self.form_check(1045),
            skip_ff: self.form_check(1035),
        }
    }

    // ---- chip database / selection ----------------------------------------------------------

    fn open_select_dialog(&mut self) -> Option<(Win, Vec<Win>)> {
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.post(&frame, WM_COMMAND, 1030);
        self.wait_dialog(&known, 1219, Duration::from_secs(6))
    }

    fn list_chips(&mut self) -> Vec<ChipEntry> {
        let Some((dlg, tree)) = self.open_select_dialog() else {
            self.log(Level::Error, tr("无法打开芯片库", "Cannot open the chip database"));
            return Vec::new();
        };
        let find = |id: i32| tree.iter().find(|w| w.id == id).map(|w| w.hwnd.clone()).unwrap_or_default();
        let (mf, ch, ed) = (find(1219), find(1220), find(1033));
        self.call(&format!("settext {ed} "));
        std::thread::sleep(Duration::from_millis(400));
        let mfrs = self.lb(&mf);
        let mut all = Vec::new();
        for (i, m) in mfrs.iter().enumerate() {
            self.call(&format!("lbpick {mf} {i}"));
            std::thread::sleep(Duration::from_millis(60));
            for name in self.lb(&ch) {
                if !name.is_empty() {
                    all.push(ChipEntry { name, mfr: m.clone() });
                }
            }
            self.progress(Some((i + 1) as f32 / mfrs.len() as f32), trf!("正在建立芯片库 {}/{}", "Building the chip database {}/{}", i + 1, mfrs.len()));
        }
        self.close_dialog(&dlg.hwnd);
        all
    }

    fn select_chip(&mut self, chip: &ChipEntry) -> bool {
        let Some((dlg, tree)) = self.open_select_dialog() else {
            self.log(Level::Error, tr("无法打开芯片选择", "Cannot open chip selection"));
            return false;
        };
        let find = |id: i32| tree.iter().find(|w| w.id == id).map(|w| w.hwnd.clone()).unwrap_or_default();
        let (mf, ch, ed, ok) = (find(1219), find(1220), find(1033), find(1));
        self.call(&format!("settext {ed} "));
        std::thread::sleep(Duration::from_millis(350));
        let mfrs = self.lb(&mf);
        let Some(mi) = mfrs.iter().position(|m| *m == chip.mfr) else {
            self.log(Level::Error, trf!("芯片库中找不到厂商 {}", "Manufacturer not found in the database: {}", chip.mfr));
            self.close_dialog(&dlg.hwnd);
            return false;
        };
        self.call(&format!("lbpick {mf} {mi}"));
        std::thread::sleep(Duration::from_millis(150));
        let chips = self.lb(&ch);
        let Some(ci) = chips.iter().position(|c| *c == chip.name) else {
            self.log(Level::Error, trf!("找不到芯片 {}", "Chip not found: {}", chip.name));
            self.close_dialog(&dlg.hwnd);
            return false;
        };
        self.call(&format!("lbpick {ch} {ci}"));
        std::thread::sleep(Duration::from_millis(150));
        self.click(&ok);
        std::thread::sleep(Duration::from_millis(500));
        // the dialog should be gone now
        if self.dialogs().iter().any(|d| d.hwnd == dlg.hwnd) {
            self.close_dialog(&dlg.hwnd);
            return false;
        }
        true
    }

    // ---- buffer exchange --------------------------------------------------------------------

    /// Hands `data` to the official program and checks that its buffer now really holds it: the checksum the
    /// official window shows must equal the byte sum of `data`, padded with 0xFF up to the chip size.
    /// Any failure is an error; the caller must then not start the operation.
    fn push_buffer(&mut self, data: &[u8]) -> Result<(), String> {
        if !self.load_buffer(data) {
            return Err(tr("缓冲区没能交给编程引擎", "The buffer could not be handed to the engine").into());
        }
        let st = self.read_state();
        let shown = st.checksum.trim().trim_start_matches("0x").trim_start_matches("0X").replace(' ', "");
        let Ok(official) = u32::from_str_radix(&shown, 16) else {
            return Err(trf!("读不出引擎缓冲区的校验和（{}）", "Cannot read the engine buffer's checksum ({})", st.checksum));
        };
        let pad = st.size.saturating_sub(data.len() as u64);
        let ours = data.iter().fold(0u32, |a, b| a.wrapping_add(*b as u32)).wrapping_add((pad as u32).wrapping_mul(0xFF));
        if official != ours {
            return Err(trf!(
                "引擎缓冲区的校验和 0x{:08X} 与界面数据 0x{:08X} 不一致",
                "The engine buffer's checksum 0x{:08X} does not match the data on screen (0x{:08X})",
                official,
                ours
            ));
        }
        Ok(())
    }

    fn load_buffer(&mut self, data: &[u8]) -> bool {
        let path = temp_file("minipro_ui_buffer.bin");
        if std::fs::write(&path, data).is_err() {
            self.log(Level::Error, tr("无法写入临时文件", "Cannot write the temporary file"));
            return false;
        }
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.call(&format!("arm-open 0 {}", path.display()));
        self.post(&frame, WM_COMMAND, 57601);
        let Some((dlg, tree)) = self.wait_dialog(&known, 1059, Duration::from_secs(6)) else {
            self.call("disarm");
            let _ = std::fs::remove_file(&path);
            self.log(Level::Error, tr("载入选项窗口没有出现", "The file-load options window did not appear"));
            return false;
        };
        // make sure: binary format, Code Memory
        for id in [1059, 1065] {
            if let Some(w) = tree.iter().find(|w| w.id == id) {
                if self.call(&format!("check {}", w.hwnd)).first().map(|s| s.trim()) != Some("1") {
                    self.click(&w.hwnd);
                }
            }
        }
        if let Some(ok) = tree.iter().find(|w| w.id == 1) {
            self.click(&ok.hwnd);
        }
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if !self.dialogs().iter().any(|d| d.hwnd == dlg.hwnd) {
                let _ = std::fs::remove_file(&path);
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.close_dialog(&dlg.hwnd);
        false
    }

    /// Saves the official buffer to a temp file and reads it back.  The file is taken only once it is complete:
    /// as long as the chip size (`expected`) when that is known, and in any case unchanged between two polls.
    fn fetch_buffer(&mut self, expected: u64) -> Option<Vec<u8>> {
        let path = temp_file("minipro_ui_dump.bin");
        let _ = std::fs::remove_file(&path);
        let frame = self.frame.clone();
        self.call(&format!("arm-save 0 {}", path.display()));
        self.post(&frame, WM_COMMAND, 1007);
        let t0 = Instant::now();
        let mut last: Option<u64> = None;
        while t0.elapsed() < Duration::from_secs(20) && !self.broken {
            std::thread::sleep(Duration::from_millis(200));
            let Ok(m) = std::fs::metadata(&path) else { continue };
            let len = m.len();
            if len > 0 && (expected == 0 || len == expected) && last == Some(len) {
                let data = std::fs::read(&path).ok().filter(|d| d.len() as u64 == len);
                let _ = std::fs::remove_file(&path);
                return data;
            }
            last = Some(len);
        }
        self.call("disarm");
        let _ = std::fs::remove_file(&path);
        None
    }

    // ---- options ------------------------------------------------------------------------------

    fn set_option(&mut self, opt: OptionId, on: bool) {
        let id = opt.control();
        if let Some(h) = self.form.get(&id).cloned() {
            let now = self.call(&format!("check {h}")).first().map(|s| s.trim() == "1").unwrap_or(false);
            if now != on {
                self.click(&h);
                std::thread::sleep(Duration::from_millis(80));
            }
        }
    }

    // ---- operations -----------------------------------------------------------------------------

    /// The picture the official operation dialog paints in its "Location In Socket" box (group box 1198): how the
    /// chip or adapter goes into the ZIF socket.  The hidden dialog renders itself into a bitmap (PrintWindow in the
    /// shim), then it is cancelled again without starting anything.
    fn socket_image(&mut self) -> Option<(usize, usize, Vec<u8>)> {
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.call("stats-reset");
        self.post(&frame, WM_COMMAND, 1002); // the read dialog: same picture, nothing starts until its OK is clicked
        let (dlg, tree) = self.wait_dialog(&known, 1198, Duration::from_secs(5))?;
        std::thread::sleep(Duration::from_millis(250));
        let boxh = tree.iter().find(|w| w.id == 1198).map(|w| w.hwnd.clone())?;
        let path = temp_file("minipro_ui_socket.raw");
        let _ = std::fs::remove_file(&path);
        // development aid: MINIPRO_UI_SOCKET_FULL captures the whole official dialog instead of the box
        let boxh = if std::env::var_os("MINIPRO_UI_SOCKET_FULL").is_some() { "0".to_string() } else { boxh };
        let reply = self.call(&format!("capture {} {} {}", dlg.hwnd, boxh, path.display())).join(" ");
        let stats = self.call("stats").join(" ");
        self.close_dialog(&dlg.hwnd);
        if std::env::var_os("MINIPRO_UI_SCRIPT").is_some() {
            self.log(Level::Info, format!("[test] socket capture: {reply} | usb while the dialog was open: {stats}"));
        }
        let raw = std::fs::read(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        if raw.len() < 8 {
            return None;
        }
        let w = u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize;
        let h = u32::from_le_bytes(raw[4..8].try_into().ok()?) as usize;
        if w == 0 || h == 0 || raw.len() < 8 + w * h * 4 {
            return None;
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for px in raw[8..8 + w * h * 4].chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        Some((w, h, rgba))
    }

    /// Open the official About dialog (menu 57664) hidden, read its two info lines, close it.
    fn read_about(&mut self) -> Option<(String, String, String)> {
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.post(&frame, WM_COMMAND, 57664);
        let (dlg, tree) = self.wait_dialog(&known, 1142, Duration::from_secs(4))?;
        let get = |id: i32| tree.iter().find(|w| w.id == id).map(|w| w.hwnd.clone());
        let line1 = get(1141).map(|h| self.text(&h)).unwrap_or_default();
        let line2 = get(1142).map(|h| self.text(&h)).unwrap_or_default();
        if let Some(ok) = get(1) {
            self.click(&ok);
            std::thread::sleep(Duration::from_millis(200));
        } else {
            self.close_dialog(&dlg.hwnd);
        }
        let dash_only = |s: &str| s.chars().all(|c| c == '-' || c == ' ');
        let mut code = String::new();
        let mut serial = String::new();
        if let Some(i) = line2.find("Serial:") {
            serial = line2[i + 7..].trim().to_string();
            code = line2[..i].replace("DEV Code:", "").trim().to_string();
        }
        if dash_only(&code) {
            code.clear();
        }
        if dash_only(&serial) {
            serial.clear();
        }
        Some((line1.trim().to_string(), code, serial))
    }

    /// The official "Programmer Self-Check" dialog (menu 32808).  Only its "开始自检" button (id 1) is used;
    /// the factory-test and voltage-test buttons (1148/1149) are never touched.
    fn run_selfcheck(&mut self) -> (bool, String) {
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.call("events");
        self.post(&frame, WM_COMMAND, 32808);
        let Some((dlg, tree)) = self.wait_dialog(&known, 1147, Duration::from_secs(6)) else {
            return (false, tr("官方程序没有打开自检窗口", "The official program did not open the self-check window").into());
        };
        let go = tree.iter().find(|w| w.id == 1).map(|w| w.hwnd.clone()).unwrap_or_default();
        let logl = tree.iter().find(|w| w.id == 1147).map(|w| w.hwnd.clone()).unwrap_or_default();
        let mut all: Vec<String> = Vec::new();
        // the list box already holds the first two lines when the dialog opens: read it from line 0
        let mut seen = 0usize;
        self.click(&go);
        let t0 = Instant::now();
        let mut started = false;
        let mut settle = 0;
        loop {
            std::thread::sleep(Duration::from_millis(150));
            if self.broken {
                return (false, engine_lost().into());
            }
            let enabled = self.is_enabled(&go);
            let lines = self.lb(&logl);
            if !enabled || lines.len() > seen {
                started = true;
            }
            for l in lines.iter().skip(seen) {
                all.push(l.clone());
            }
            seen = seen.max(lines.len());
            if started {
                self.progress(None, tr("自检中…", "Self-check running…").to_string());
            }
            if started && enabled {
                settle += 1;
                if settle >= 4 {
                    break;
                }
            } else {
                settle = 0;
            }
            if !started && t0.elapsed() > Duration::from_secs(6) {
                self.close_dialog(&dlg.hwnd);
                return (false, tr("官方程序没有开始自检", "The official program did not start the self-check").into());
            }
            if t0.elapsed() > Duration::from_secs(180) {
                break;
            }
        }
        for e in self.call("events") {
            if let Some(i) = e.find("MessageBox") {
                self.log(Level::Info, e[i..].replace("MessageBox ", ""));
            }
        }
        self.close_dialog(&dlg.hwnd);
        if all.iter().any(|l| l.contains("Device unavailable")) {
            return (false, no_device().into());
        }
        // one line per group ("VPP Testing......" followed by No.xx lines); single-line checks stand alone
        let mut group: Option<(String, usize, usize)> = None; // (title, ok, bad)
        let mut report: Vec<(bool, String)> = Vec::new();
        let flush = |g: &mut Option<(String, usize, usize)>, r: &mut Vec<(bool, String)>| {
            if let Some((t, ok, bad)) = g.take() {
                r.push((bad == 0, trf!("{t}：{ok}/{} 通过", "{t}: {ok}/{} passed", ok + bad)));
            }
        };
        for l in &all {
            if l.starts_with("No.") {
                if let Some(g) = group.as_mut() {
                    if l.contains("OK") { g.1 += 1 } else { g.2 += 1; report.push((false, format!("{} {l}", g.0))); }
                }
            } else if l.trim().is_empty() {
            } else if l.contains("TEST FINISHED") {
                flush(&mut group, &mut report);
            } else if l.contains("OK!") || l.to_lowercase().contains("fail") {
                flush(&mut group, &mut report);
                report.push((l.contains("OK!"), l.trim_end_matches('.').replace("......", tr("：", ":")).replace("...", tr("：", ":"))));
            } else {
                flush(&mut group, &mut report);
                group = Some((l.trim_end_matches('.').to_string(), 0, 0));
            }
        }
        flush(&mut group, &mut report);
        for (ok, line) in &report {
            self.log(if *ok { Level::Ok } else { Level::Error }, line.clone());
        }
        let bad = all.iter().any(|l| {
            let low = l.to_lowercase();
            low.contains("fail") || low.contains("error") || low.contains("失败") || low.contains("错误") || low.contains("异常")
        });
        let items = all.iter().filter(|l| l.contains("OK")).count();
        let fails = all.iter().filter(|l| l.starts_with("No.") && !l.contains("OK")).count();
        let verdict = all.iter().rev().find(|l| l.contains("TEST FINISHED")).cloned().unwrap_or_else(|| tr("没有结论", "no verdict").into());
        let summary = trf!("共 {} 项通过，{} 项异常 — {}", "{} checks passed, {} failed — {}", items, fails, verdict.trim_matches(|c| c == '<' || c == '>'));
        (!bad && fails == 0 && !all.is_empty(), summary)
    }

    fn run_op(&mut self, op: Op) -> (bool, String) {
        let size = self.read_state().size.max(1);
        let known = self.known_dialogs();
        let frame = self.frame.clone();
        self.call("events");
        self.post(&frame, WM_COMMAND, op.menu());
        let Some((dlg, tree)) = self.wait_dialog(&known, 1056, Duration::from_secs(6)) else {
            if self.broken {
                return (false, engine_lost().into());
            }
            return (false, tr("官方程序没有打开操作窗口", "The official program did not open the operation window").into());
        };
        let go = tree.iter().find(|w| w.id == 1).map(|w| w.hwnd.clone()).unwrap_or_default();
        let status = tree.iter().find(|w| w.id == 1056).map(|w| w.hwnd.clone()).unwrap_or_default();
        let logl = tree.iter().find(|w| w.id == 1000).map(|w| w.hwnd.clone()).unwrap_or_default();
        let mut seen_lines = 0usize;
        // lines already there before we start (chip id check from opening the dialog)
        let first = self.lb(&logl);
        for l in &first {
            self.log(Level::Info, l.clone());
        }
        seen_lines = seen_lines.max(first.len());

        self.call("stats-reset");
        let initial_status = self.text(&status);
        self.click(&go);
        let t0 = Instant::now();
        let mut was_busy = false;
        let mut started = false;
        let mut retried = false;
        let mut last_status = String::new();
        let mut extra_dialogs: Vec<String> = known.clone();
        extra_dialogs.push(dlg.hwnd.clone());
        let mut mismatches: Vec<String> = Vec::new();
        let mut final_wait = 0;
        loop {
            std::thread::sleep(Duration::from_millis(110));
            if self.broken {
                return (false, engine_lost().into());
            }
            let enabled = self.is_enabled(&go);
            if !enabled {
                was_busy = true;
            }
            let st = self.text(&status);
            let lines = self.lb(&logl);
            if was_busy || st != initial_status || lines.len() > seen_lines {
                started = true;
            }
            if !started {
                // any USB traffic since the click means the operation did start, just slowly
                let stats = self.call("stats").join(" ");
                let ops: u64 = ["in_ops=", "out_ops="]
                    .iter()
                    .map(|k| stats.split_whitespace().find_map(|p| p.strip_prefix(k)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0))
                    .sum();
                if ops > 0 {
                    started = true;
                }
            }
            if !started {
                if !retried && t0.elapsed() > Duration::from_millis(3000) {
                    retried = true;
                    self.log(Level::Warn, tr("操作窗口没有响应，重新点击一次", "The operation window did not respond, clicking again"));
                    self.dismiss_stray_dialogs();
                    self.click(&go);
                } else if t0.elapsed() > Duration::from_secs(6) {
                    self.close_dialog(&dlg.hwnd);
                    return (false, tr("官方程序没有开始执行这个操作", "The official program did not start this operation").into());
                }
                continue;
            }
            for l in lines.iter().skip(seen_lines) {
                self.log(Level::Info, l.clone());
            }
            seen_lines = seen_lines.max(lines.len());

            // progress from real USB traffic
            let stats = self.call("stats").join(" ");
            let get = |k: &str| -> u64 {
                stats.split_whitespace().find_map(|p| p.strip_prefix(k)).and_then(|v| v.parse().ok()).unwrap_or(0)
            };
            let frac = match op {
                Op::Read | Op::Verify => Some((get("in_bytes=") as f32 / size as f32).min(0.99)),
                Op::Program => Some((get("out_bytes=") as f32 / size as f32).min(0.99)),
                _ => None,
            };
            if st != last_status {
                last_status = st.clone();
            }
            self.progress(frac, last_status.clone());

            // extra dialogs: verify mismatch etc.
            for d in self.dialogs() {
                if extra_dialogs.contains(&d.hwnd) {
                    continue;
                }
                extra_dialogs.push(d.hwnd.clone());
                let t = self.tree(&d.hwnd);
                let body: Vec<String> = t
                    .iter()
                    .filter(|w| w.class == "Static" && !w.text.is_empty())
                    .map(|w| w.text.replace("\\n", " · "))
                    .collect();
                let body = body.join(" ");
                if t.iter().any(|w| w.id == 1046) {
                    mismatches.push(body.clone());
                    self.log(Level::Warn, trf!("发现差异: {body}", "Difference found: {body}"));
                    self.close_dialog(&d.hwnd); // stop at the first difference
                } else {
                    self.log(Level::Warn, trf!("官方程序提示: {} {body}", "Official program says: {} {body}", d.text));
                    if let Some(ok) = t.iter().find(|w| w.id == 1) {
                        self.click(&ok.hwnd);
                    } else {
                        self.close_dialog(&d.hwnd);
                    }
                }
            }
            if enabled && started {
                // finished: buttons are enabled again; give the final text a moment to settle
                final_wait += 1;
                if final_wait >= 3 {
                    break;
                }
            } else {
                final_wait = 0;
            }
            if t0.elapsed() > Duration::from_secs(20 * 60) {
                break;
            }
        }
        let final_status = self.text(&status);
        let lines = self.lb(&logl);
        for l in lines.iter().skip(seen_lines) {
            self.log(Level::Info, l.clone());
        }
        // some operations show their result only through message boxes (reported as events)
        for e in self.call("events") {
            if let Some(i) = e.find("MessageBox") {
                self.log(Level::Info, e[i..].replace("MessageBox ", ""));
            }
        }
        self.close_dialog(&dlg.hwnd);
        if final_status.contains("Device unavailable") || lines.iter().any(|l| l.contains("Device unavailable")) {
            return (false, no_device().into());
        }
        let low = final_status.to_lowercase();
        if op == Op::Blank {
            // official texts: "Device is Blank!" / "Device is not Blank!" (also seen in the dialog's list box)
            let all = format!("{low} {}", lines.join(" ").to_lowercase());
            let not_blank = all.contains("not blank") || all.contains("not empty") || all.contains("isn't blank");
            if not_blank {
                return (false, tr("芯片不是空白的", "Chip is not blank").into());
            }
            if low.contains("blank") || low.contains("empty") {
                return (true, tr("芯片是空白的", "Chip is blank").into());
            }
        }
        let ok = mismatches.is_empty()
            && !low.contains("fail")
            && !low.contains("error")
            && !low.contains("not empty")
            && !low.contains("differ")
            && (low.contains("success") || low.contains("empty") || low.contains("ok") || low.contains("complete") || low.contains("finish"));
        let summary = if !mismatches.is_empty() { format!("{final_status} — {}", mismatches[0]) } else { final_status };
        (ok, summary)
    }
}

// ---------------------------------------------------------------------------------------------
// worker thread
// ---------------------------------------------------------------------------------------------

pub fn spawn(ctx: egui::Context) -> (Sender<Job>, Receiver<Event>) {
    let (jtx, jrx) = channel::<Job>();
    let (etx, erx) = channel::<Event>();
    std::thread::spawn(move || {
        let mut engine = match Engine::start(etx.clone(), ctx.clone()) {
            Ok(e) => e,
            Err(msg) => {
                let _ = etx.send(Event::Fatal(msg));
                ctx.request_repaint();
                return;
            }
        };
        let st = engine.read_state();
        engine.send(Event::State(st));
        engine.send(Event::Ready);
        while let Ok(job) = jrx.recv() {
            engine.send(Event::Busy(true));
            if !matches!(job, Job::Quit) {
                engine.dismiss_stray_dialogs();
            }
            match job {
                Job::Quit => {
                    engine.shutdown(6000);
                    engine.send(Event::Quitted);
                    break;
                }
                Job::Refresh => {
                    let st = engine.read_state();
                    engine.send(Event::State(st));
                }
                Job::ListChips => {
                    let list = engine.list_chips();
                    engine.send(Event::ChipList(list));
                }
                Job::SelectChip(chip) => {
                    engine.progress(None, trf!("正在切换芯片 {}", "Switching chip {}", chip.name));
                    let ok = engine.select_chip(&chip);
                    let st = engine.read_state();
                    if ok {
                        engine.log(Level::Ok, trf!("已选择芯片 {}", "Chip selected: {}", st.name));
                    } else {
                        engine.log(Level::Error, trf!("切换芯片失败: {}", "Failed to switch chip: {}", chip.name));
                    }
                    engine.send(Event::State(st));
                }
                Job::Identify => {
                    engine.progress(None, tr("正在识别芯片 ID…", "Reading the chip ID…"));
                    let frame = engine.frame.clone();
                    engine.post(&frame, WM_COMMAND, 32798);
                    std::thread::sleep(Duration::from_millis(1800));
                    let st = engine.read_state();
                    if st.chip_id.is_empty() {
                        engine.log(Level::Warn, tr("没有读到芯片 ID（检查芯片是否插好）", "No chip ID was read (check that the chip is inserted correctly)"));
                    } else {
                        engine.log(Level::Ok, trf!("芯片 ID: {}", "Chip ID: {}", st.chip_id));
                    }
                    engine.send(Event::State(st));
                }
                Job::Restart => {
                    engine.log(Level::Info, tr("编程器重新连接，正在重启编程引擎…", "Programmer reconnected, restarting the engine…"));
                    engine.progress(None, tr("正在重启编程引擎…", "Restarting the engine…"));
                    // the old process must really be gone before a new one is started (it holds the pipe)
                    engine.shutdown(8000);
                    std::thread::sleep(Duration::from_millis(500));
                    match Engine::start(etx.clone(), ctx.clone()) {
                        Ok(e) => {
                            engine = e;
                            let st = engine.read_state();
                            engine.send(Event::State(st));
                            engine.send(Event::Restarted);
                            engine.log(Level::Ok, tr("编程引擎已重启", "Engine restarted"));
                        }
                        Err(msg) => {
                            engine.send(Event::Fatal(msg));
                            break;
                        }
                    }
                }
                Job::SocketImage => {
                    let img = engine.socket_image();
                    engine.send(Event::Socket(img));
                }
                Job::ReadInfo => {
                    if let Some((v, c, s)) = engine.read_about() {
                        if !c.is_empty() || !s.is_empty() {
                            engine.log(Level::Info, trf!("编程器信息: {v} | 设备码 {c} | 序列号 {s}", "Programmer: {v} | device code {c} | serial {s}"));
                        }
                        engine.send(Event::Programmer(v, c, s));
                    }
                }
                Job::SelfCheck => {
                    engine.log(Level::Info, tr("—— 编程器自检 ——", "—— Programmer self-check ——"));
                    engine.progress(None, tr("编程器自检…", "Programmer self-check…"));
                    let (ok, summary) = engine.run_selfcheck();
                    engine.send(Event::OpDone { op: tr("编程器自检", "Programmer self-check"), ok, summary });
                }
                Job::Run(op, data) => {
                    engine.log(Level::Info, format!("—— {} ——", op.label()));
                    let handed = match &data {
                        None => Ok(()),
                        Some(d) => {
                            engine.progress(None, tr("正在把缓冲区交给编程引擎…", "Handing the buffer to the engine…"));
                            let r = engine.push_buffer(d);
                            if r.is_ok() {
                                engine.log(Level::Info, trf!("缓冲区已交给引擎并核对无误 ({} 字节)", "Buffer handed to the engine and checked ({} bytes)", crate::app::with_commas(d.len() as u64)));
                            }
                            r
                        }
                    };
                    engine.progress(Some(0.0), format!("{}…", op.label()));
                    let (ok, summary) = match handed {
                        Ok(()) => engine.run_op(op),
                        // never run on a buffer that is not the one on screen
                        Err(msg) => (false, trf!("{}，操作已取消", "{}; the operation was not started", msg)),
                    };
                    engine.send(Event::OpDone { op: op.label(), ok, summary });
                    let st = engine.read_state();
                    let size = st.size;
                    engine.send(Event::State(st));
                    if op == Op::Read && ok {
                        match engine.fetch_buffer(size) {
                            Some(d) => engine.send(Event::Buffer(d)),
                            None => engine.log(
                                Level::Error,
                                tr("读取成功，但没能把数据取回界面；屏幕上仍是之前的内容", "The read succeeded, but the data could not be brought into the window; the screen still shows the previous content"),
                            ),
                        }
                    }
                }
                Job::FetchBuffer => {
                    let size = engine.read_state().size;
                    if let Some(d) = engine.fetch_buffer(size) {
                        engine.send(Event::Buffer(d));
                    }
                }
                Job::SetOption(opt, on) => {
                    engine.set_option(opt, on);
                    let st = engine.read_state();
                    engine.send(Event::State(st));
                }
            }
            if engine.broken {
                // the hidden engine crashed or exited: start a new one instead of reporting fake empty results
                engine.log(Level::Info, tr("正在重新启动编程引擎…", "Restarting the programming engine…"));
                engine.shutdown(2000);
                std::thread::sleep(Duration::from_millis(500));
                match Engine::start(etx.clone(), ctx.clone()) {
                    Ok(e) => {
                        engine = e;
                        let st = engine.read_state();
                        engine.send(Event::State(st));
                        engine.send(Event::Restarted);
                        engine.log(Level::Ok, tr("编程引擎已重启", "Engine restarted"));
                    }
                    Err(msg) => {
                        engine.send(Event::Fatal(msg));
                        break;
                    }
                }
            }
            engine.progress(Some(0.0), "");
            engine.send(Event::Busy(false));
        }
    });
    (jtx, erx)
}
