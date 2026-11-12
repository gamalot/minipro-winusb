#![windows_subsystem = "windows"]

mod app;
mod engine;
mod fileio;
mod hex;
mod lang;
mod socket;
mod theme;
mod usb;

use eframe::egui;

/// A second front end would start a second hidden engine that cannot get the control pipe; refuse instead.
fn already_running() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\minipro-ui-single-instance\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        // the handle is kept for the lifetime of the process on purpose
        !h.is_null() && GetLastError() == ERROR_ALREADY_EXISTS
    }
}

fn main() -> eframe::Result {
    if already_running() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                w("MiniPro is already running.\n\nMiniPro 已经在运行。").as_ptr(),
                w("MiniPro").as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MiniPro")
            .with_decorations(false) // own title bar that follows the theme
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("icon"))
            .with_inner_size([1310.0, 900.0])
            .with_min_inner_size([1300.0, 780.0]),
        ..Default::default()
    };
    eframe::run_native("MiniPro", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
