//! Starts the official MiniPro.exe with `minipro_winusb.dll` injected.
//!
//! usage: minipro-launch.exe [args passed to MiniPro.exe]
//! Put this launcher and minipro_winusb.dll into the MiniPro install directory; it starts the
//! MiniPro.exe that sits next to it.  Both are 32-bit, like MiniPro.exe.

// GUI subsystem: no console window flashes up.  Errors are reported with a message box instead.
#![windows_subsystem = "windows"]

#[cfg(not(target_pointer_width = "32"))]
compile_error!("minipro-launch must be built for a 32-bit target (i686-pc-windows-msvc), like MiniPro.exe");

use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThread, GetExitCodeThread, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

/// One argument quoted for a Windows command line (the rules CommandLineToArgvW / the MSVC runtime use).
fn quote_arg(a: &OsStr) -> OsString {
    let s = a.to_string_lossy();
    if !s.is_empty() && !s.contains([' ', '\t', '\n', '"']) {
        return a.to_os_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0usize;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    OsString::from(out)
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn fail(msg: &str) -> ! {
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(OsStr::new(msg)).as_ptr(),
            wide(OsStr::new("MiniPro WinUSB launcher")).as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1);
}

fn main() {
    // The launcher lives in the MiniPro install directory and only looks there: the target is
    // MiniPro.exe and the DLL is minipro_winusb.dll, both next to the launcher itself.
    // Any arguments are passed to MiniPro.exe as-is.
    let dir: PathBuf = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| fail("cannot locate own directory"));
    let target = dir.join("MiniPro.exe");
    let dll = dir.join("minipro_winusb.dll");
    let mut rest: Vec<OsString> = std::env::args_os().skip(1).collect();
    // `--headless`: keep the official window hidden and expose the control pipe (consumed here, not passed on)
    if let Some(i) = rest.iter().position(|a| a == "--headless") {
        rest.remove(i);
        std::env::set_var("MINIPRO_WINUSB_HEADLESS", "1");
    }
    if !target.is_file() {
        fail(&format!("{} not found (put the launcher in the MiniPro install directory)", target.display()));
    }
    if !dll.is_file() {
        fail(&format!("{} not found (it must sit next to the launcher)", dll.display()));
    }

    // command line: "target" args...
    let mut cmd = OsString::from("\"");
    cmd.push(&target);
    cmd.push("\"");
    for a in &rest {
        cmd.push(" ");
        cmd.push(quote_arg(a));
    }
    let mut cmd_w = wide(&cmd);
    let cwd_w = wide(target.parent().unwrap_or(&target).as_os_str());

    unsafe {
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        if CreateProcessW(
            std::ptr::null(),
            cmd_w.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_SUSPENDED,
            std::ptr::null(),
            cwd_w.as_ptr(),
            &si,
            &mut pi,
        ) == 0
        {
            let e = GetLastError();
            fail(&format!(
                "CreateProcess failed (error {e}){}",
                if e == 740 { " - MiniPro.exe needs elevation: run this launcher as administrator" } else { "" }
            ));
        }

        let dll_w = wide(dll.as_os_str());
        let bytes = dll_w.len() * 2;
        let remote = VirtualAllocEx(pi.hProcess, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        let mut ok = !remote.is_null();
        if ok {
            ok = WriteProcessMemory(pi.hProcess, remote, dll_w.as_ptr() as *const c_void, bytes, std::ptr::null_mut()) != 0;
        }
        let mut loaded = false;
        if ok {
            let k32 = GetModuleHandleW(wide(OsStr::new("kernel32.dll")).as_ptr());
            let load = GetProcAddress(k32, b"LoadLibraryW\0".as_ptr());
            let th = CreateRemoteThread(
                pi.hProcess,
                std::ptr::null(),
                0,
                std::mem::transmute(load),
                remote,
                0,
                std::ptr::null_mut(),
            );
            if !th.is_null() {
                if WaitForSingleObject(th, 15_000) == WAIT_OBJECT_0 {
                    let mut code = 0u32;
                    GetExitCodeThread(th, &mut code);
                    loaded = code != 0;
                }
                CloseHandle(th);
            }
        }
        if !remote.is_null() {
            VirtualFreeEx(pi.hProcess, remote, 0, MEM_RELEASE);
        }
        if !loaded {
            TerminateProcess(pi.hProcess, 1);
            CloseHandle(pi.hThread);
            CloseHandle(pi.hProcess);
            fail("could not inject minipro_winusb.dll into MiniPro.exe");
        }
        ResumeThread(pi.hThread);
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
}
