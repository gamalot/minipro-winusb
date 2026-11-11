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

fn main() -> eframe::Result {
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
