use crate::lang::{tr, trf};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontFamily, Frame, Layout, Margin, RichText, Sense, Stroke, Vec2};

use crate::engine::{self, ChipEntry, ChipState, Event, Job, Level, Op, OptionId};
use crate::hex::HexView;
use crate::theme::{self, Palette};
use crate::fileio;

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join("MiniProUI");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn load_chip_cache() -> Vec<ChipEntry> {
    let Ok(text) = std::fs::read_to_string(data_dir().join("chips.tsv")) else { return Vec::new() };
    text.lines()
        .filter_map(|l| l.split_once('\t').map(|(m, n)| ChipEntry { mfr: m.to_string(), name: n.to_string() }))
        .collect()
}

fn save_chip_cache(list: &[ChipEntry]) {
    let text: String = list.iter().map(|c| format!("{}\t{}\n", c.mfr, c.name)).collect();
    let _ = std::fs::write(data_dir().join("chips.tsv"), text);
}

/// A titled rounded card.  Free function (not a method) so its closure may freely borrow the app.
/// 262144 -> "262,144"
pub fn with_commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "App Software Ver: 6.85   Firmware Ver: 03.2.86   MODEL: TL866CS" -> (software, firmware, model)
fn split_version(s: &str) -> (String, String, String) {
    let field = |key: &str, end: &[&str]| -> String {
        let Some(i) = s.find(key) else { return String::new() };
        let rest = &s[i + key.len()..];
        let stop = end.iter().filter_map(|e| rest.find(e)).min().unwrap_or(rest.len());
        rest[..stop].trim().to_string()
    };
    (
        field("Software Ver:", &["Firmware", "MODEL"]),
        field("Firmware Ver:", &["MODEL", "App"]),
        field("MODEL:", &["App", "Firmware"]),
    )
}

/// Shared look of every dialog: dimmed backdrop, rounded card in the theme's colours, title and close button in
/// one header row.  Returns true when the dialog should close (close button, click outside, Esc).
fn modal(ctx: &egui::Context, pal: &Palette, id: &str, title: &str, width: f32, add: impl FnOnce(&mut egui::Ui)) -> bool {
    let mut close = false;
    let frame = Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(26))
        .shadow(egui::Shadow { offset: [0, 10], blur: 32, spread: 0, color: Color32::from_black_alpha(if pal.dark { 120 } else { 50 }) });
    let resp = egui::Modal::new(egui::Id::new(id))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(if pal.dark { 130 } else { 70 }))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).size(20.0).strong().color(pal.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let (rect, r) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
                    if r.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(8), pal.stroke);
                    }
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "\u{E8BB}",
                        egui::FontId::new(12.0, FontFamily::Name("icons".into())),
                        if r.hovered() { pal.text } else { pal.dim },
                    );
                    close = r.clicked();
                });
            });
            ui.add_space(14.0);
            add(ui);
        });
    close || resp.should_close()
}

/// Segmented control: equal-width segments in one rounded track.  Returns the index that was clicked.
fn segmented(ui: &mut egui::Ui, pal: &Palette, width: f32, items: &[(&str, bool)]) -> Option<usize> {
    let h = 36.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    ui.painter().rect(rect, CornerRadius::same(10), pal.bg, Stroke::new(1.0, pal.stroke), egui::StrokeKind::Inside);
    let seg_w = (width - 6.0) / items.len() as f32;
    let mut clicked = None;
    for (i, (label, on)) in items.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + 3.0 + seg_w * i as f32, rect.top() + 3.0), Vec2::new(seg_w, h - 6.0));
        let resp = ui.interact(r, ui.id().with(("seg", *label, i)), Sense::click());
        if *on {
            ui.painter().rect_filled(r, CornerRadius::same(7), pal.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(7), pal.stroke);
        }
        let col = if *on { pal.accent_text } else { pal.text };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *label, egui::FontId::proportional(14.5), col);
        if resp.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// Left-hand label of one settings row.
fn setting_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(Vec2::new(110.0, 36.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(Vec2::new(110.0, 36.0));
        ui.label(RichText::new(text).size(15.0));
    });
}

/// Button row at the bottom of a dialog, right-aligned: `(primary clicked, cancel clicked)`.
fn dialog_buttons(ui: &mut egui::Ui, pal: &Palette, primary: &str, fill: Color32) -> (bool, bool) {
    let (mut go, mut cancel) = (false, false);
    ui.add_space(20.0);
    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 40.0), Layout::right_to_left(Align::Center), |ui| {
        let b = egui::Button::new(RichText::new(primary).size(15.0).strong().color(Color32::WHITE))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(9))
            .min_size(Vec2::new(150.0, 40.0));
        go = ui.add(b).clicked();
        let c = egui::Button::new(RichText::new(tr("取消", "Cancel")).size(15.0).color(pal.text))
            .corner_radius(CornerRadius::same(9))
            .min_size(Vec2::new(104.0, 40.0));
        cancel = ui.add(c).clicked();
    });
    (go, cancel)
}

fn card(pal: &Palette, ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(pal.card)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 2.0; // compact rows so the three cards fit a 900 px window
            ui.label(RichText::new(title).size(13.5).strong().color(pal.dim));
            ui.add_space(2.0);
            add(ui);
        });
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Danger,
    Normal,
}

pub struct App {
    tx: Sender<Job>,
    rx: Receiver<Event>,
    dark: bool,
    /// follow the Windows light/dark setting
    theme_auto: bool,
    pal: Palette,

    ready: bool,
    busy: bool,
    fatal: Option<String>,
    /// programmer present on the USB bus (checked every second, independent of the engine)
    connected: bool,
    last_usb: Instant,
    /// (official software line, device code, serial number)
    prog: (String, String, String),
    info_wanted: bool,
    info_tries: u32,
    last_info: Instant,
    needs_restart: bool,
    settings: bool,
    /// UI scale (egui zoom factor), saved in the settings
    zoom: f32,
    zoom_resize: u8,
    /// window size (in points) to restore after a zoom change, so the layout keeps its proportions
    zoom_target: Vec2,
    icon_tex: Option<egui::TextureHandle>,
    /// development aid: the picture captured from the official dialog (script steps `socket`, `socketpng`)
    socket: Option<(usize, usize, Vec<u8>)>,
    /// the placement diagram is open
    socket_view: bool,
    /// development aid: chip names whose diagrams are drawn side by side (script step `socketgrid:a|b|c`)
    socket_grid: Vec<String>,
    about: bool,

    chip: ChipState,
    chips: Vec<ChipEntry>,
    building_chips: bool,
    picker: bool,
    search: String,
    results: Vec<usize>,
    results_for: String,

    hex: HexView,
    synced: bool,
    file_name: Option<String>,

    log: Vec<(f32, Level, String)>,
    t0: Instant,
    progress: (Option<f32>, String),
    pending: Option<Op>,
    pending_selfcheck: bool,
    goto_text: String,
    find_text: String,
    find_ascii: bool,

    /// self-test script from MINIPRO_UI_SCRIPT, e.g. `wait:2;shot:a.png;read;idle;shot:b.png;quit`
    script: Vec<String>,
    script_i: usize,
    script_wait: Option<Instant>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let saved = std::fs::read_to_string(data_dir().join("settings.txt")).unwrap_or_default();
        // language: English unless "lang=zh" was saved (must be set before the engine thread starts)
        let lang_en = !saved.contains("lang=zh");
        crate::lang::set_en(lang_en);
        // theme: no saved choice yet (or "theme=auto") means follow the system; "dark=1/0" is the old format
        let fixed_dark = saved.contains("dark=1") || saved.contains("theme=dark");
        let fixed_light = saved.contains("dark=0") || saved.contains("theme=light");
        let theme_auto = !(fixed_dark || fixed_light);
        let dark = if theme_auto {
            cc.egui_ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(true)
        } else {
            fixed_dark
        };
        let pal = theme::palette(dark);
        theme::apply(&cc.egui_ctx, &pal);
        let zoom = saved
            .lines()
            .find_map(|l| l.strip_prefix("zoom="))
            .and_then(|v| v.trim().parse::<f32>().ok())
            .filter(|z| (0.5..=2.5).contains(z))
            .unwrap_or(1.0);
        cc.egui_ctx.set_zoom_factor(zoom);
        let (tx, rx) = engine::spawn(cc.egui_ctx.clone());
        let chips = load_chip_cache();
        App {
            tx,
            rx,
            dark,
            theme_auto,
            pal,
            ready: false,
            busy: true,
            fatal: None,
            connected: crate::usb::programmer_present(),
            last_usb: Instant::now(),
            prog: Default::default(),
            info_wanted: true,
            info_tries: 0,
            last_info: Instant::now(),
            needs_restart: false,
            settings: false,
            zoom,
            zoom_resize: 2,
            zoom_target: Vec2::new(1310.0, 900.0),
            icon_tex: None,
            socket: None,
            socket_view: false,
            socket_grid: Vec::new(),
            about: false,
            chip: ChipState::default(),
            chips,
            building_chips: false,
            picker: false,
            search: String::new(),
            results: Vec::new(),
            results_for: "\u{0}".into(),
            hex: HexView::default(),
            synced: true,
            file_name: None,
            log: Vec::new(),
            t0: Instant::now(),
            progress: (Some(0.0), tr("正在启动编程引擎…", "Starting the engine…").into()),
            pending: None,
            pending_selfcheck: false,
            goto_text: String::new(),
            find_text: String::new(),
            find_ascii: false,
            script: std::env::var("MINIPRO_UI_SCRIPT")
                .map(|v| v.split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default(),
            script_i: 0,
            script_wait: None,
        }
    }

    /// Runs the self-test script one step per frame.  Screenshots are taken by egui itself (this
    /// application's own framebuffer only), no desktop capture and no synthetic mouse input.
    fn run_script(&mut self, ctx: &egui::Context) {
        if self.script_i >= self.script.len() {
            return;
        }
        if let Some(t) = self.script_wait {
            if Instant::now() < t {
                return;
            }
            self.script_wait = None;
        }
        let step = self.script[self.script_i].clone();
        let (cmd, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
        let needs_idle = matches!(cmd, "read" | "identify" | "verify" | "blank" | "idle" | "picker" | "select" | "program" | "erase" | "savebuf" | "load" | "selfcheck" | "restart" | "socket");
        if needs_idle && (!self.ready || self.busy) {
            return;
        }
        self.script_i += 1;
        match cmd {
            "wait" => self.script_wait = Some(Instant::now() + Duration::from_secs_f32(arg.parse().unwrap_or(1.0))),
            "shot" => {
                let path = arg.to_string();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    let _ = image::save_buffer(&path, &rgba, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgba8);
                })));
            }
            "read" => self.start_op(Op::Read),
            "verify" => self.start_op(Op::Verify),
            "blank" => self.start_op(Op::Blank),
            "restart" => {
                self.busy = true;
                self.prog = Default::default();
                let _ = self.tx.send(Job::Restart);
            }
            "identify" => {
                self.busy = true;
                let _ = self.tx.send(Job::Identify);
            }
            // self-test only: the person running the test confirmed the ZIF socket is empty
            "selfcheck" => {
                self.busy = true;
                let _ = self.tx.send(Job::SelfCheck);
            }
            // self-test only: open the settings window / change the zoom without the mouse
            "find" => {
                let h: String = arg.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                let pat: Vec<u8> = (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()).collect();
                let found = self.hex.find(&pat);
                self.say(Level::Info, format!("[test] find {arg}: {found} at 0x{:X}", self.hex.cursor));
            }
            "settings" => self.settings = arg != "off",
            "about" => self.about = arg != "off",
            "socket" => {
                self.busy = true;
                let _ = self.tx.send(Job::SocketImage);
            }
            // development aid: save the picture captured from the official dialog (for comparing with our drawing)
            "socketpng" => {
                if let Some((w, h, rgba)) = &self.socket {
                    let _ = image::save_buffer(arg, rgba, *w as u32, *h as u32, image::ColorType::Rgba8);
                }
            }
            "socketgrid" => self.socket_grid = arg.split('|').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect(),
            "socketview" => self.socket_view = arg != "off",
            "askselfcheck" => self.pending_selfcheck = arg != "off",
            "confirm" => self.pending = if arg == "off" { None } else { Some(Op::Program) },
            "zoom" => ctx.set_zoom_factor(arg.parse().unwrap_or(1.0)),
            "lang" => self.set_language(arg != "zh"),
            "theme" if arg == "auto" => self.set_auto(ctx),
            "theme" => self.set_dark(ctx, arg != "light"),
            "picker" => {
                self.picker = true;
                self.search = arg.to_string();
            }
            "closepicker" => self.picker = false,
            // self-test only: the person running the test has agreed to the hardware write
            "program" => self.run(Op::Program),
            "erase" => self.run(Op::Erase),
            "savebuf" => {
                match fileio::save(std::path::Path::new(arg), &self.hex.data) {
                    Ok(()) => self.say(Level::Ok, trf!("[自检] 缓冲区已保存到 {arg}", "[test] buffer saved to {arg}")),
                    Err(e) => self.say(Level::Error, trf!("[自检] 保存失败: {e}", "[test] save failed: {e}")),
                }
            }
            "load" => match fileio::load(std::path::Path::new(arg)) {
                Ok(d) => {
                    self.say(Level::Ok, trf!("[自检] 已载入 {arg} ({} 字节)", "[test] loaded {arg} ({} bytes)", with_commas(d.len() as u64)));
                    self.hex.set_data(d);
                    self.synced = false;
                }
                Err(e) => self.say(Level::Error, trf!("[自检] 载入失败: {e}", "[test] load failed: {e}")),
            },
            "select" => {
                if let Some(c) = self.chips.iter().find(|c| c.name == arg).cloned() {
                    self.busy = true;
                    let _ = self.tx.send(Job::SelectChip(c));
                }
            }
            "goto" => {
                if let Ok(a) = usize::from_str_radix(arg.trim_start_matches("0x"), 16) {
                    self.hex.goto(a);
                }
            }
            "quit" => {
                if let Some(path) = std::env::var_os("MINIPRO_UI_LOGFILE") {
                    let text: String = self.log.iter().map(|(t, l, m)| format!("{t:>6.1}s [{l:?}] {m}\n")).collect();
                    let _ = std::fs::write(path, text);
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close)
            }
            _ => {}
        }
    }

    fn say(&mut self, level: Level, msg: impl Into<String>) {
        self.log.push((self.t0.elapsed().as_secs_f32(), level, msg.into()));
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }

    fn apply_dark(&mut self, ctx: &egui::Context, dark: bool) {
        self.dark = dark;
        self.pal = theme::palette(dark);
        theme::apply(ctx, &self.pal);
    }

    /// Fixed light or dark theme.
    fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        self.theme_auto = false;
        self.apply_dark(ctx, dark);
        self.save_settings();
    }

    /// Follow the Windows light/dark setting (re-checked every frame, so it changes live).
    fn set_auto(&mut self, ctx: &egui::Context) {
        self.theme_auto = true;
        let dark = ctx.system_theme().map(|t| t == egui::Theme::Dark).unwrap_or(self.dark);
        self.apply_dark(ctx, dark);
        self.save_settings();
    }

    fn save_settings(&self) {
        let theme = if self.theme_auto { "auto" } else if self.dark { "dark" } else { "light" };
        let lang = if crate::lang::is_en() { "en" } else { "zh" };
        let _ = std::fs::write(data_dir().join("settings.txt"), format!("theme={theme}\nlang={lang}\nzoom={}\n", self.zoom));
    }

    fn set_language(&mut self, en: bool) {
        crate::lang::set_en(en);
        self.save_settings();
    }

    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if !self.theme_auto {
            return;
        }
        if let Some(t) = ctx.system_theme() {
            let dark = t == egui::Theme::Dark;
            if dark != self.dark {
                self.apply_dark(ctx, dark);
            }
        }
    }

    /// The window has no system frame, so the edges and corners are resize handles.
    fn resize_handles(&self, ctx: &egui::Context) {
        use egui::{CursorIcon as C, ResizeDirection as D};
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            return;
        }
        let rect = ctx.content_rect();
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let m = 6.0;
        let l = p.x - rect.min.x < m;
        let r = rect.max.x - p.x < m;
        let t = p.y - rect.min.y < m;
        let b = rect.max.y - p.y < m;
        let hit = match (l, r, t, b) {
            (true, _, true, _) => Some((D::NorthWest, C::ResizeNorthWest)),
            (_, true, true, _) => Some((D::NorthEast, C::ResizeNorthEast)),
            (true, _, _, true) => Some((D::SouthWest, C::ResizeSouthWest)),
            (_, true, _, true) => Some((D::SouthEast, C::ResizeSouthEast)),
            (true, ..) => Some((D::West, C::ResizeWest)),
            (_, true, ..) => Some((D::East, C::ResizeEast)),
            (_, _, true, _) => Some((D::North, C::ResizeNorth)),
            (_, _, _, true) => Some((D::South, C::ResizeSouth)),
            _ => None,
        };
        if let Some((dir, cursor)) = hit {
            ctx.set_cursor_icon(cursor);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }
    }

    fn poll(&mut self) {
        while let Ok(e) = self.rx.try_recv() {
            match e {
                Event::Log(l, m) => {
                    if let Some(rest) = m.strip_prefix("ID: 0x") {
                        let id: String = rest.split("...").next().unwrap_or("").trim().to_string();
                        if !id.is_empty() {
                            self.chip.chip_id = id;
                        }
                    }
                    self.say(l, m)
                }
                Event::Progress { frac, text } => self.progress = (frac, text),
                Event::State(s) => self.chip = s,
                Event::Buffer(d) => {
                    let sum: u64 = d.iter().map(|b| *b as u64).sum();
                    let ff = d.iter().filter(|b| **b == 0xFF).count();
                    self.say(Level::Ok, trf!("已读入缓冲区 {} 字节（累加和 0x{sum:X}，其中 0xFF 占 {} 字节）", "Read {} bytes into the buffer (sum 0x{sum:X}, {} bytes are 0xFF)",
                        with_commas(d.len() as u64),
                        with_commas(ff as u64)
                    ));
                    self.hex.set_data(d);
                    self.synced = true;
                    self.file_name = None;
                }
                Event::ChipList(list) => {
                    self.building_chips = false;
                    if !list.is_empty() {
                        self.say(Level::Ok, trf!("芯片库已建立，共 {} 个型号", "Chip database built: {} chips", list.len()));
                        save_chip_cache(&list);
                        self.chips = list;
                        self.results_for = "\u{0}".into();
                    }
                }
                Event::OpDone { op, ok, summary } => {
                    self.say(if ok { Level::Ok } else { Level::Error }, format!(
                        "{op}{}{}: {summary}",
                        if crate::lang::is_en() { " " } else { "" },
                        if ok { tr("完成", "complete") } else { tr("未通过", "failed") }
                    ));
                    if ok && op == Op::Program.label() {
                        self.hex.mark_clean();
                    }
                }
                Event::Ready => {
                    self.ready = true;
                    self.busy = false;
                    self.progress = (Some(0.0), String::new());
                    self.say(Level::Ok, tr("编程引擎已就绪", "Engine ready"));
                    if self.chips.is_empty() {
                        self.building_chips = true;
                        let _ = self.tx.send(Job::ListChips);
                    }
                }
                Event::Programmer(v, c, s) => self.prog = (v, c, s),
                Event::Socket(img) => {
                    if let (Some((w, h, rgba)), Some(path)) = (&img, std::env::var_os("MINIPRO_UI_SOCKET_PNG")) {
                        let _ = image::save_buffer(&path, rgba, *w as u32, *h as u32, image::ColorType::Rgba8);
                    }
                    self.socket = img;
                }
                Event::Restarted => {
                    // the new engine has an empty buffer: push ours again before the next write/verify
                    self.synced = false;
                    self.info_wanted = true;
                    self.info_tries = 0;
                }
                Event::Busy(b) => self.busy = b,
                Event::Fatal(m) => {
                    self.fatal = Some(m);
                    self.busy = false;
                }
            }
        }
    }

    /// Operations are possible: engine up, idle, and the programmer really plugged in.
    fn can(&self) -> bool {
        self.ready && !self.busy && self.connected
    }

    /// Once a second: is the programmer still on the bus?  Reads its info again after a re-plug.
    fn watch_usb(&mut self) {
        if self.last_usb.elapsed() < Duration::from_millis(1000) {
            return;
        }
        self.last_usb = Instant::now();
        let now = crate::usb::programmer_present();
        if now != self.connected {
            self.connected = now;
            self.info_tries = 0;
            if now {
                self.say(Level::Ok, tr("编程器已连接", "Programmer connected"));
                self.info_wanted = true;
                // the official program never re-opens a device that appeared (or re-appeared) after it
                // started, so it is re-launched once the programmer is back
                self.needs_restart = true;
            } else {
                self.say(Level::Error, tr("编程器已断开，请检查 USB 连接", "Programmer disconnected, check the USB connection"));
                self.prog = Default::default();
            }
        }
        if self.connected && self.ready && !self.busy {
            if self.needs_restart {
                self.needs_restart = false;
                self.busy = true;
                self.prog = Default::default();
                let _ = self.tx.send(Job::Restart);
                return;
            }
            let have = !self.prog.1.is_empty() || !self.prog.2.is_empty();
            if have {
                self.info_tries = 0;
            } else if (self.info_wanted || self.last_info.elapsed() > Duration::from_secs(2)) && self.info_tries < 15 {
                // the official program may need a moment after a (re)plug before it knows the device
                self.info_wanted = false;
                self.info_tries += 1;
                self.last_info = Instant::now();
                let _ = self.tx.send(Job::ReadInfo);
                if self.info_tries == 15 {
                    self.say(Level::Warn, tr("读不到编程器信息（序列号等），可以重启本程序再试", "Cannot read the programmer info (serial number etc.), try restarting the program"));
                }
            }
        }
    }

    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let pal = self.pal;
        if self.icon_tex.is_none() {
            if let Ok(img) = image::load_from_memory(include_bytes!("../assets/icon.png")) {
                let rgba = img.to_rgba8();
                let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
                self.icon_tex = Some(ctx.load_texture("app-icon", ci, egui::TextureOptions::LINEAR));
            }
        }
        let close = modal(ctx, &pal, "about", tr("关于", "About"), 480.0, |ui| {
            ui.horizontal(|ui| {
                if let Some(tex) = &self.icon_tex {
                    ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), Vec2::splat(60.0))));
                    ui.add_space(8.0);
                }
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(RichText::new("MiniPro").size(26.0).strong().color(pal.accent));
                    ui.label(RichText::new(trf!("WinUSB 新界面 · 版本 {}", "WinUSB front end · version {}", env!("CARGO_PKG_VERSION"))).size(13.5).color(pal.dim));
                });
            });
            ui.add_space(14.0);
            ui.label(
                RichText::new(tr(
                    "后端仍是官方 MiniPro v6.85（隐藏运行），USB 通过 WinUSB 垫片访问，只替换了外观。",
                    "The backend is still the official MiniPro v6.85, running hidden. USB goes through a WinUSB shim; only the look is replaced.",
                ))
                .size(14.0)
                .color(pal.dim),
            );
            ui.add_space(16.0);
            Frame::new().fill(pal.bg).stroke(Stroke::new(1.0, pal.stroke)).corner_radius(CornerRadius::same(12)).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (sw, fw, model) = split_version(&self.prog.0);
                self.kv(ui, tr("型号", "Model"), &model, false);
                self.kv(ui, tr("固件", "Firmware"), &fw, true);
                self.kv(ui, tr("软件", "Software"), &sw, true);
                self.kv(ui, tr("设备码", "Device code"), &self.prog.1, true);
                self.kv(ui, tr("序列号", "Serial no."), &self.prog.2, true);
                self.kv(ui, "USB", if self.connected { tr("已连接 (04D8:E11C)", "Connected (04D8:E11C)") } else { tr("未连接", "Not connected") }, false);
            });
        });
        if close {
            self.about = false;
        }
    }

    fn start_op(&mut self, op: Op) {
        if matches!(op, Op::Program | Op::Erase) {
            self.pending = Some(op);
        } else {
            self.run(op);
        }
    }

    fn run(&mut self, op: Op) {
        if matches!(op, Op::Program | Op::Verify) {
            if self.hex.data.is_empty() {
                self.say(Level::Warn, tr("缓冲区为空：请先读取芯片或打开文件", "Buffer is empty: read the chip or open a file first"));
                return;
            }
            if self.chip.size > 0 && self.hex.data.len() as u64 > self.chip.size {
                self.say(Level::Error, trf!("数据 {} 字节超过芯片容量 {} 字节", "Data of {} bytes exceeds the chip capacity of {} bytes",
                        with_commas(self.hex.data.len() as u64),
                        with_commas(self.chip.size)
                    ));
                return;
            }
            if !self.synced {
                let _ = self.tx.send(Job::LoadBuffer(self.hex.data.clone()));
                self.synced = true;
            }
        }
        self.busy = true;
        let _ = self.tx.send(Job::Run(op));
    }

    fn open_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(tr("芯片数据 (*.bin, *.hex)", "Chip data (*.bin, *.hex)"), &["bin", "hex", "rom", "img"])
            .add_filter(tr("所有文件", "All files"), &["*"])
            .pick_file()
        else {
            return;
        };
        match fileio::load(&path) {
            Ok(d) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.say(Level::Ok, trf!("已打开 {name} ({} 字节)", "Opened {name} ({} bytes)", with_commas(d.len() as u64)));
                self.hex.set_data(d);
                self.synced = false;
                self.file_name = Some(name);
            }
            Err(e) => self.say(Level::Error, trf!("打开失败: {e}", "Open failed: {e}")),
        }
    }

    fn save_file(&mut self) {
        if self.hex.data.is_empty() {
            self.say(Level::Warn, tr("缓冲区为空，没有可保存的数据", "The buffer is empty, nothing to save"));
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter(tr("二进制 (*.bin)", "Binary (*.bin)"), &["bin"])
            .add_filter("Intel HEX (*.hex)", &["hex"])
            .set_file_name(format!("{}.bin", self.chip.name.split(' ').next().unwrap_or("chip")))
            .save_file()
        else {
            return;
        };
        match fileio::save(&path, &self.hex.data) {
            Ok(()) => self.say(Level::Ok, trf!("已保存 {}", "Saved {}", path.display())),
            Err(e) => self.say(Level::Error, trf!("保存失败: {e}", "Save failed: {e}")),
        }
    }

    // ---------------------------------------------------------------------------------------------
    // small widgets
    // ---------------------------------------------------------------------------------------------

    fn big_button(&self, ui: &mut egui::Ui, label: &str, hint: &str, kind: Kind, enabled: bool) -> bool {
        let p = &self.pal;
        let (fill, fg, stroke) = match kind {
            Kind::Primary => (p.accent, p.accent_text, p.accent),
            Kind::Danger => (p.err.gamma_multiply(if p.dark { 0.85 } else { 1.0 }), Color32::WHITE, p.err),
            Kind::Normal => (p.card, p.text, p.stroke),
        };
        let b = egui::Button::new(RichText::new(label).size(17.0).strong().color(fg))
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(ui.available_width(), 46.0));
        ui.add_enabled(enabled, b).on_hover_text(hint).clicked()
    }

    fn kv(&self, ui: &mut egui::Ui, key: &str, value: &str, mono: bool) {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(104.0, 22.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(Vec2::new(104.0, 22.0)); // fixed key column, text left-aligned
                ui.label(RichText::new(key).color(self.pal.dim));
            });
            if value.is_empty() {
                // same placeholder, same (proportional) font on every row
                ui.label(RichText::new("—").color(self.pal.dim));
            } else if mono {
                ui.label(RichText::new(value).monospace());
            } else {
                ui.label(value);
            }
        });
    }

    // ---------------------------------------------------------------------------------------------
    // panels
    // ---------------------------------------------------------------------------------------------

    /// Like `kv`, with a small button at the right end of the row; returns true when it was clicked.
    fn kv_action(&self, ui: &mut egui::Ui, key: &str, value: &str, mono: bool, label: &str, icon_font: bool, tip: &str, enabled: bool) -> bool {
        let mut clicked = false;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(104.0, 26.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_size(Vec2::new(104.0, 26.0));
                ui.label(RichText::new(key).color(self.pal.dim));
            });
            // button flush right, the value takes whatever is left of the row
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut text = RichText::new(label).size(16.0).color(self.pal.text);
                if icon_font {
                    text = text.family(FontFamily::Name("icons".into()));
                }
                let b = egui::Button::new(text).min_size(Vec2::new(34.0, 26.0)).corner_radius(CornerRadius::same(7));
                clicked = ui.add_enabled(enabled, b).on_hover_text(tip).clicked();
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 26.0), Layout::left_to_right(Align::Center), |ui| {
                    if value.is_empty() {
                        ui.label(RichText::new("—").color(self.pal.dim));
                    } else {
                        let t = if mono { RichText::new(value).monospace() } else { RichText::new(value) };
                        // long chip names wrap onto a second line instead of being cut off
                        ui.add(egui::Label::new(t).wrap()).on_hover_text(value);
                    }
                });
            });
        });
        clicked
    }

    /// Minimize / maximize / close button of the custom title bar.
    fn window_button(&self, ui: &mut egui::Ui, glyph: &str, is_close: bool) -> bool {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(46.0, 40.0), Sense::click());
        let p = &self.pal;
        if resp.hovered() {
            let fill = if is_close { p.err } else { p.stroke };
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        }
        let col = if is_close && resp.hovered() { Color32::WHITE } else { p.text };
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            egui::FontId::new(14.0, FontFamily::Name("icons".into())),
            col,
        );
        resp.clicked()
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Panel::top("top")
            .exact_size(78.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(22, 14)).stroke(Stroke::new(1.0, pal.stroke)))
            .show(ui, |ui| {
                // the whole bar is the window's title bar: drag to move, double-click to maximize
                let ctx = ui.ctx().clone();
                let drag = ui.interact(ui.max_rect(), ui.id().with("titlebar"), Sense::click_and_drag());
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                if drag.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("MiniPro").size(26.0).strong().color(pal.accent));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.window_button(ui, "\u{E8BB}", true) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if self.window_button(ui, if maximized { "\u{E923}" } else { "\u{E922}" }, false) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if self.window_button(ui, "\u{E921}", false) {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        ui.add_space(14.0);
                        let gear = RichText::new("\u{E713}").family(FontFamily::Name("icons".into())).size(26.0).color(pal.text);
                        if ui.add(egui::Button::new(gear).min_size(Vec2::new(46.0, 40.0))).on_hover_text(tr("设置", "Settings")).clicked() {
                            self.settings = !self.settings;
                        }
                    });
                });
            });
    }

    /// Settings dialog: theme, language and UI scale, one segmented control per row.
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings {
            return;
        }
        let pal = self.pal;
        const CW: f32 = 390.0;
        let close = modal(ctx, &pal, "settings", tr("设置", "Settings"), 110.0 + 8.0 + CW, |ui| {
            ui.horizontal(|ui| {
                setting_label(ui, tr("主题", "Theme"));
                let items = [
                    (tr("跟随系统", "Follow system"), self.theme_auto),
                    (tr("浅色", "Light"), !self.theme_auto && !self.dark),
                    (tr("深色", "Dark"), !self.theme_auto && self.dark),
                ];
                match segmented(ui, &pal, CW, &items) {
                    Some(0) => self.set_auto(ctx),
                    Some(1) => self.set_dark(ctx, false),
                    Some(2) => self.set_dark(ctx, true),
                    _ => {}
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                setting_label(ui, tr("语言", "Language"));
                let en = crate::lang::is_en();
                match segmented(ui, &pal, CW, &[("English", en), ("中文", !en)]) {
                    Some(0) => self.set_language(true),
                    Some(1) => self.set_language(false),
                    _ => {}
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                setting_label(ui, tr("缩放", "Zoom"));
                let zs = [0.8f32, 0.9, 1.0, 1.1, 1.25, 1.5];
                let labels: Vec<String> = zs.iter().map(|z| format!("{:.0}%", z * 100.0)).collect();
                let items: Vec<(&str, bool)> = labels.iter().zip(zs).map(|(l, z)| (l.as_str(), (self.zoom - z).abs() < 0.01)).collect();
                if let Some(i) = segmented(ui, &pal, CW, &items) {
                    ctx.set_zoom_factor(zs[i]);
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(118.0);
                ui.label(RichText::new(tr("也可以按 Ctrl + 加号 / 减号", "Ctrl + plus / minus works too")).size(12.5).color(pal.dim));
            });
        });
        if close {
            self.settings = false;
        }
    }

    /// Keeps `self.zoom` in step with egui's zoom factor (settings window or Ctrl +/-), saves it, and makes sure
    /// the window is still large enough for the layout at the new scale.
    fn track_zoom(&mut self, ctx: &egui::Context) {
        let z = ctx.zoom_factor();
        if (z - self.zoom).abs() > 0.001 {
            // the window keeps its size in points: everything gets bigger or smaller, the layout stays the same
            self.zoom_target = ctx.content_rect().size() * (z / self.zoom);
            self.zoom = z;
            self.zoom_resize = 2;
            self.save_settings();
        }
        if self.zoom_resize > 0 {
            self.zoom_resize -= 1;
            if self.zoom_resize == 0 {
                let min = Vec2::new(1300.0, 780.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
                let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                if !maximized {
                    let mut want = self.zoom_target;
                    if let Some(mon) = ctx.input(|i| i.viewport().monitor_size) {
                        want = want.min(mon - Vec2::new(0.0, 48.0)); // not larger than the screen (minus the taskbar)
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want.max(min)));
                }
            }
        }
    }

    /// Square icon button for the tool rail; the name and a hint show on hover.
    fn icon_button(&self, ui: &mut egui::Ui, glyph: &str, name: &str, hint: &str, enabled: bool) -> bool {
        let p = &self.pal;
        let b = egui::Button::new(RichText::new(glyph).family(FontFamily::Name("icons".into())).size(26.0).color(p.text))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(10))
            .min_size(Vec2::new(52.0, 52.0));
        ui.add_enabled(enabled, b)
            .on_hover_ui(|ui| {
                ui.label(RichText::new(name).strong().size(15.0));
                ui.label(RichText::new(hint).color(p.dim));
            })
            .clicked()
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Panel::left("ops")
            .exact_size(76.0)
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(12)).stroke(Stroke::new(1.0, pal.stroke)))
            .show(ui, |ui| {
                let can = self.can();
                if self.icon_button(ui, "\u{E8E5}", tr("打开文件", "Open file"), tr("载入 .bin 或 .hex 到缓冲区", "Load a .bin or .hex file into the buffer"), true) {
                    self.open_file();
                }
                if self.icon_button(ui, "\u{E74E}", tr("保存缓冲区到文件", "Save buffer to file"), tr("把缓冲区保存为 .bin 或 .hex", "Save the buffer as .bin or .hex"), true) {
                    self.save_file();
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                if self.icon_button(ui, "\u{E898}", tr("读取芯片", "Read chip"), tr("把芯片内容读到缓冲区", "Read the chip into the buffer"), can) {
                    self.start_op(Op::Read);
                }
                if self.icon_button(ui, "\u{E896}", tr("编程芯片", "Program chip"), tr("把缓冲区写入芯片（会先确认）", "Write the buffer to the chip (asks first)"), can) {
                    self.start_op(Op::Program);
                }
                if self.icon_button(ui, "\u{E73E}", tr("校验", "Verify"), tr("比较芯片与缓冲区", "Compare the chip with the buffer"), can) {
                    self.start_op(Op::Verify);
                }
                if self.icon_button(ui, "\u{E75C}", tr("擦除", "Erase"), tr("擦除整片芯片（会先确认）", "Erase the whole chip (asks first)"), can) {
                    self.start_op(Op::Erase);
                }
                if self.icon_button(ui, "\u{E7C3}", tr("空白检查", "Blank check"), tr("检查芯片是否为空", "Check whether the chip is blank"), can) {
                    self.start_op(Op::Blank);
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                if self.icon_button(ui, "\u{E9D9}", tr("编程器自检", "Programmer self-check"), tr("检测编程器本身；需先取出插座中的芯片", "Tests the programmer itself; remove the chip from the socket first"), can) {
                    self.pending_selfcheck = true;
                }
                if self.icon_button(ui, "\u{E946}", tr("关于", "About"), tr("版本与编程器信息", "Version and programmer information"), true) {
                    self.about = true;
                }
            });
    }

    /// Operation log across the bottom, like STM32CubeProgrammer.
    fn log_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Panel::bottom("log")
            .exact_size(190.0)
            .resizable(false)
            .frame(Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(16, 8)))
            .show(ui, |ui| {
                // same width as the buffer card above it
                let card_w = (crate::hex::HexView::table_width(ui) + 28.0 + 10.0).min(ui.available_width());
                ui.allocate_ui_with_layout(Vec2::new(card_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
                Frame::new()
                    .fill(pal.card)
                    .stroke(Stroke::new(1.0, pal.stroke))
                    .corner_radius(CornerRadius::same(12))
                    .inner_margin(Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_height(ui.available_height());
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr("操作记录", "Log")).size(13.5).strong().color(pal.dim));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(tr("清空", "Clear")).clicked() {
                                    self.log.clear();
                                }
                            });
                        });
                        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                            for (t, lvl, msg) in &self.log {
                                let col = match lvl {
                                    Level::Info => pal.text,
                                    Level::Ok => pal.ok,
                                    Level::Warn => pal.warn,
                                    Level::Error => pal.err,
                                };
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(RichText::new(format!("{:>5.1}s", t)).monospace().size(12.5).color(pal.dim));
                                    ui.label(RichText::new(msg).size(14.5).color(col));
                                });
                            }
                        });
                    });
                });
            });
    }

    fn right_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Panel::right("info")
            .exact_size(450.0)
            .resizable(false)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::same(16)).stroke(Stroke::new(1.0, pal.stroke)))
            .show(ui, |ui| {
              egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                card(&pal, ui, tr("编程器", "Programmer"), |ui| {
                    let (txt, col) = if self.fatal.is_some() {
                        (tr("● 引擎未连接", "● Engine not connected"), pal.err)
                    } else if !self.connected {
                        (tr("● 未连接，请检查 USB 连接", "● Not connected, check the USB cable"), pal.err)
                    } else if !self.ready {
                        (tr("● 已连接，引擎启动中", "● Connected, engine starting"), pal.warn)
                    } else if self.busy {
                        (tr("● 已连接，工作中", "● Connected, busy"), pal.warn)
                    } else {
                        (tr("● 已连接，就绪", "● Connected, ready"), pal.ok)
                    };
                    ui.label(RichText::new(txt).size(16.0).strong().color(col));
                    ui.add_space(4.0);
                    self.kv(ui, tr("设备码", "Device code"), &self.prog.1, true);
                    self.kv(ui, tr("序列号", "Serial no."), &self.prog.2, true);
                    let (_sw, fw, model) = split_version(&self.prog.0);
                    let both = if model.is_empty() { String::new() } else { trf!("{model}   固件 {fw}", "{model}   firmware {fw}") };
                    self.kv(ui, tr("型号", "Model"), &both, false);
                });
                ui.add_space(4.0);
                card(&pal, ui, tr("芯片信息", "Chip"), |ui| {
                    let can = self.can();
                    if self.kv_action(ui, tr("型号", "Model"), &self.chip.name, false, "…", false, tr("选择芯片型号", "Choose the chip model"), can) {
                        self.picker = true;
                    }
                    self.kv(ui, tr("类型", "Type"), &self.chip.kind, false);
                    let cap = if self.chip.size > 0 {
                        trf!("{} 字节（0x{:X}）", "{} bytes (0x{:X})", with_commas(self.chip.size), self.chip.size)
                    } else {
                        self.chip.size_text.clone()
                    };
                    self.kv(ui, tr("容量", "Capacity"), &cap, false);
                    let pl = crate::socket::placement(&self.chip.name);
                    if self.kv_action(
                        ui,
                        tr("插入方式", "Placement"),
                        &pl.short(),
                        false,
                        "\u{E8B9}",
                        true,
                        tr("显示芯片或转接座在 ZIF 插座中的位置", "Show how the chip or adapter goes into the ZIF socket"),
                        pl != crate::socket::Placement::Unknown,
                    ) {
                        self.socket_view = true;
                    }
                    if self.kv_action(ui, tr("芯片 ID", "Chip ID"), &self.chip.chip_id, true, "\u{E72C}", true, tr("重新读取芯片 ID", "Read the chip ID again"), can) {
                        self.busy = true;
                        let _ = self.tx.send(Job::Identify);
                    }
                    self.kv(ui, tr("校验和", "Checksum"), &self.chip.checksum, true);
                });
                ui.add_space(4.0);
                card(&pal, ui, tr("编程选项", "Options"), |ui| {
                    let can = self.can();
                    let opts: [(&str, Option<bool>, OptionId); 5] = [
                        (tr("编程前先擦除", "Erase before programming"), self.chip.erase_before, OptionId::EraseBefore),
                        (tr("编程后校验", "Verify after programming"), self.chip.verify_after, OptionId::VerifyAfter),
                        (tr("编程前查空", "Blank check before programming"), self.chip.blank_before, OptionId::BlankBefore),
                        (tr("检查芯片 ID", "Check chip ID"), self.chip.check_id, OptionId::CheckId),
                        (tr("跳过写入 0xFF", "Skip writing 0xFF"), self.chip.skip_ff, OptionId::SkipFf),
                    ];
                    // larger boxes with a clearly visible border, and a gap between box and text
                    {
                        let st = ui.style_mut();
                        st.spacing.icon_width = 20.0;
                        st.spacing.icon_width_inner = 12.0;
                        st.spacing.icon_spacing = 12.0;
                        st.spacing.item_spacing.y = 6.0;
                        let border = Stroke::new(1.6, pal.dim);
                        let tick = Stroke::new(2.0, pal.accent);
                        st.visuals.widgets.inactive.bg_stroke = border;
                        st.visuals.widgets.hovered.bg_stroke = Stroke::new(1.8, pal.accent);
                        st.visuals.widgets.active.bg_stroke = Stroke::new(1.8, pal.accent);
                        st.visuals.widgets.inactive.fg_stroke = tick;
                        st.visuals.widgets.hovered.fg_stroke = tick;
                        st.visuals.widgets.active.fg_stroke = tick;
                    }
                    for (label, state, id) in opts {
                        let mut v = state.unwrap_or(false);
                        let changed = ui.add_enabled(can && state.is_some(), egui::Checkbox::new(&mut v, label)).changed();
                        if changed {
                            self.busy = true;
                            let _ = self.tx.send(Job::SetOption(id, v));
                        }
                    }
                });
              });
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::Panel::bottom("footer")
            .exact_size(52.0)
            .frame(Frame::new().fill(pal.panel).inner_margin(Margin::symmetric(22, 10)).stroke(Stroke::new(1.0, pal.stroke)))
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let idle = !self.busy && self.progress.1.is_empty();
                    let text = if idle { tr("就绪", "Ready").to_string() } else { self.progress.1.clone() };
                    let w = (ui.available_width() - 300.0).max(200.0);
                    let bar = match self.progress.0 {
                        _ if idle => egui::ProgressBar::new(0.0).text(RichText::new(text).color(pal.dim)),
                        Some(f) => egui::ProgressBar::new(f).text(RichText::new(format!("{text}   {:.0}%", f * 100.0)).color(Color32::WHITE)),
                        None => egui::ProgressBar::new(0.0).animate(true).text(RichText::new(text).color(pal.text)),
                    };
                    let fill = if idle { Color32::TRANSPARENT } else { pal.accent };
                    ui.add_sized([w, 26.0], bar.fill(fill).corner_radius(CornerRadius::same(6)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let n = self.hex.data.len();
                        let mut s = trf!("缓冲区 {} 字节", "Buffer: {} bytes", with_commas(n as u64));
                        if let Some(f) = &self.file_name {
                            s = format!("{f} · {s}");
                        }
                        if self.hex.dirty {
                            s += &trf!(" · 已修改 {} 处", " · {} modified", with_commas(self.hex.modified_count() as u64));
                        }
                        ui.label(RichText::new(s).color(pal.dim));
                    });
                });
            });
    }

    fn center(&mut self, ui: &mut egui::Ui) {
        let pal = self.pal;
        egui::CentralPanel::default().frame(Frame::new().fill(pal.bg).inner_margin(Margin::same(16))).show(ui, |ui| {
            // the card hugs the hex table (no empty strip to the right of the ASCII column)
            let card_w = (crate::hex::HexView::table_width(ui) + 28.0 + 10.0).min(ui.available_width());
            ui.allocate_ui_with_layout(Vec2::new(card_w, ui.available_height()), Layout::top_down(Align::Min), |ui| {
            Frame::new()
                .fill(pal.card)
                .stroke(Stroke::new(1.0, pal.stroke))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(14))
                .show(ui, |ui| {
                    let full = ui.available_size();
                    ui.set_min_size(full - Vec2::new(28.0, 28.0));
                    // toolbar
                    // fixed row height: with `ui.horizontal` the row grows while widgets are added, so controls added
                    // early sit higher than those added later; here everything is centred on one line from the start
                    ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 36.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.spacing_mut().interact_size.y = 32.0;
                        // the CJK font has more room below the glyphs than above, so plain labels look about
                        // 3 pt too high next to the input boxes; nudge them down to the visual centre line
                        let nudged = |ui: &mut egui::Ui, text: &str, size: f32, color: Color32, dy: f32| {
                            let galley = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(size), color);
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(galley.size().x, 32.0), Sense::hover());
                            let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0 + dy);
                            ui.painter().galley(pos, galley, color);
                        };
                        nudged(ui, tr("数据", "Data"), 13.5, pal.dim, 0.5);
                        ui.add_space(12.0);
                        nudged(ui, tr("跳转到 0x", "Go to 0x"), 14.0, pal.text, 0.0);
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.goto_text)
                                .desired_width(100.0)
                                .hint_text(tr("地址", "Address"))
                                .font(egui::TextStyle::Monospace)
                                .vertical_align(Align::Center)
                                .min_size(Vec2::new(0.0, 32.0)),
                        );
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            if let Ok(a) = usize::from_str_radix(self.goto_text.trim().trim_start_matches("0x"), 16) {
                                self.hex.goto(a);
                            }
                        }
                        ui.add_space(14.0);
                        nudged(ui, tr("查找", "Find"), 14.0, pal.text, 0.0);
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.find_text)
                                .desired_width(180.0)
                                .hint_text(if self.find_ascii { tr("文本", "Text") } else { tr("十六进制 如 DE AD", "Hex, e.g. DE AD") })
                                .font(egui::TextStyle::Monospace)
                                .vertical_align(Align::Center)
                                .min_size(Vec2::new(0.0, 32.0)),
                        );
                        ui.selectable_value(&mut self.find_ascii, false, "HEX");
                        ui.selectable_value(&mut self.find_ascii, true, tr("文本", "Text"));
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button(tr("查找下一个", "Find next")).clicked() || enter {
                            let pat: Vec<u8> = if self.find_ascii {
                                self.find_text.bytes().collect()
                            } else {
                                let h: String = self.find_text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
                                (0..h.len() / 2).filter_map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()).collect()
                            };
                            if !self.hex.find(&pat) {
                                self.say(Level::Warn, tr("没有找到匹配的数据", "No matching data found"));
                            }
                        }
                    });
                    ui.add_space(2.0);
                    let keys = !ui.ctx().egui_wants_keyboard_input() && self.picker == false;
                    let footer_h = 30.0;
                    let avail = ui.available_height() - footer_h;
                    ui.allocate_ui(Vec2::new(ui.available_width(), avail.max(100.0)), |ui| {
                        self.hex.show(ui, &pal, keys);
                    });
                    // cursor details
                    ui.horizontal(|ui| {
                        if let Some(b) = self.hex.data.get(self.hex.cursor) {
                            ui.label(
                                RichText::new(trf!("地址 0x{:08X}    值 0x{:02X}    十进制 {:<3}    二进制 {:08b}", "Address 0x{:08X}    Value 0x{:02X}    Dec {:<3}    Bin {:08b}",
                                    self.hex.cursor, b, b, b
                                ))
                                .monospace()
                                .color(pal.dim),
                            );
                        }
                    });
                });
            });
        });
    }

    fn chip_picker(&mut self, ctx: &egui::Context) {
        if !self.picker {
            return;
        }
        let pal = self.pal;
        if self.results_for != self.search {
            let tokens: Vec<String> = self.search.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
            let mut scored: Vec<(u8, usize)> = Vec::new();
            for (i, c) in self.chips.iter().enumerate() {
                let n = c.name.to_lowercase();
                let hay = format!("{n} {}", c.mfr.to_lowercase());
                if tokens.iter().all(|t| hay.contains(t.as_str())) {
                    let rank = if tokens.first().map(|t| n.starts_with(t.as_str())).unwrap_or(true) { 0 } else { 1 };
                    scored.push((rank, i));
                }
            }
            scored.sort_by_key(|(r, i)| (*r, *i));
            self.results = scored.into_iter().map(|(_, i)| i).collect();
            self.results_for = self.search.clone();
        }
        let mut chosen: Option<usize> = None;
        let close = modal(ctx, &pal, "chip-picker", tr("选择芯片", "Choose chip"), 600.0, |ui| {
            if self.chips.is_empty() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(if self.building_chips { tr("正在建立芯片库，首次使用需要约半分钟…", "Building the chip database, the first run takes about half a minute…") } else { tr("芯片库尚未建立", "Chip database not built yet") }).color(pal.dim).size(17.0));
                });
                ui.add_space(30.0);
                return;
            }
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(12, 9))
                    .hint_text(tr("输入型号，例如 SST39SF020A、W25Q64、24C02、ATMEGA328", "Type a model, e.g. SST39SF020A, W25Q64, 24C02, ATMEGA328"))
                    .font(egui::FontId::proportional(17.0)),
            );
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let n = self.results.len();
            ui.add_space(4.0);
            ui.label(RichText::new(trf!("共 {} 个型号，匹配 {n} 个", "{} chips, {n} match", with_commas(self.chips.len() as u64))).color(pal.dim).size(13.0));
            ui.add_space(4.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter && n > 0 {
                chosen = Some(self.results[0]);
            }
            egui::ScrollArea::vertical().max_height(400.0).min_scrolled_height(400.0).auto_shrink([false, false]).show_rows(ui, 36.0, n.min(500), |ui, range| {
                for k in range {
                    let c = &self.chips[self.results[k]];
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
                    let p = ui.painter_at(rect);
                    if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::same(8), pal.accent.gamma_multiply(0.22));
                    } else if k % 2 == 1 {
                        p.rect_filled(rect, CornerRadius::same(8), pal.row_alt);
                    }
                    p.text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, &c.name, egui::FontId::proportional(16.0), pal.text);
                    p.text(rect.right_center() - Vec2::new(12.0, 0.0), Align2::RIGHT_CENTER, &c.mfr, egui::FontId::proportional(13.5), pal.dim);
                    if resp.clicked() {
                        chosen = Some(self.results[k]);
                    }
                }
            });
        });
        if close {
            self.picker = false;
        }
        if let Some(i) = chosen {
            let chip = self.chips[i].clone();
            self.picker = false;
            self.busy = true;
            self.search.clear();
            let _ = self.tx.send(Job::SelectChip(chip));
        }
    }

    /// The placement diagram on its own, opened from the chip card.
    fn socket_window(&mut self, ctx: &egui::Context) {
        if !self.socket_view {
            return;
        }
        let pal = self.pal;
        let pl = crate::socket::placement(&self.chip.name);
        let name = self.chip.name.split_whitespace().collect::<Vec<_>>().join(" ");
        let close = modal(ctx, &pal, "socket", tr("芯片插入方式", "Placement in the socket"), 400.0, |ui| {
            if pl.has_diagram() {
                ui.vertical_centered(|ui| {
                    crate::socket::draw(ui, &pal, pal.card, &pl, 400.0);
                });
                ui.add_space(12.0);
            }
            ui.label(RichText::new(name).size(15.5).strong());
            ui.add_space(2.0);
            ui.label(RichText::new(pl.describe()).size(14.0).color(pal.dim));
        });
        if close {
            self.socket_view = false;
        }
    }

    fn selfcheck_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_selfcheck {
            return;
        }
        let pal = self.pal;
        let mut decision: Option<bool> = None;
        let close = modal(ctx, &pal, "selfcheck", tr("编程器自检", "Programmer self-check"), 460.0, |ui| {
            ui.label(RichText::new(tr("请先取出 ZIF 插座中的芯片，并确认插座是空的。", "Remove the chip from the ZIF socket and make sure the socket is empty.")).size(15.5));
            let (go, cancel) = dialog_buttons(ui, &pal, tr("插座已空，开始自检", "Socket is empty, start"), pal.err);
            if go {
                decision = Some(true);
            } else if cancel {
                decision = Some(false);
            }
        });
        if close {
            decision = decision.or(Some(false));
        }
        if let Some(go) = decision {
            self.pending_selfcheck = false;
            if go {
                self.busy = true;
                let _ = self.tx.send(Job::SelfCheck);
            }
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(op) = self.pending else { return };
        let pal = self.pal;
        let mut decision: Option<bool> = None;
        let title = trf!("确认{}", "Confirm {}", op.label());
        let pl = crate::socket::placement(&self.chip.name);
        let close = modal(ctx, &pal, "confirm", &title, if pl.has_diagram() { 600.0 } else { 460.0 }, |ui| {
            let what = match op {
                Op::Program => trf!("即将把缓冲区的 {} 字节写入芯片 {}。\n芯片上原有的数据会被覆盖。", "About to write {} bytes from the buffer to {}.\nExisting data on the chip will be overwritten.",
                    with_commas(self.hex.data.len() as u64),
                    self.chip.name
                ),
                _ => trf!("即将擦除芯片 {}。\n芯片上的数据会全部丢失。", "About to erase {}.\nAll data on the chip will be lost.", self.chip.name),
            };
            if pl.has_diagram() {
                ui.horizontal_top(|ui| {
                    crate::socket::draw(ui, &pal, pal.card, &pl, 300.0);
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(what).size(15.5));
                        ui.add_space(12.0);
                        ui.label(RichText::new(pl.describe()).size(14.0).color(pal.dim));
                    });
                });
            } else {
                ui.label(RichText::new(what).size(15.5));
                if pl != crate::socket::Placement::Unknown {
                    ui.add_space(10.0);
                    ui.label(RichText::new(pl.describe()).size(14.0).color(pal.dim));
                }
            }
            let (go, cancel) = dialog_buttons(ui, &pal, &trf!("确定{}", "Yes, {}", op.label()), pal.err);
            if go {
                decision = Some(true);
            } else if cancel {
                decision = Some(false);
            }
        });
        if close {
            decision = decision.or(Some(false));
        }
        if let Some(go) = decision {
            self.pending = None;
            if go {
                self.run(op);
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        let ctx = ui.ctx().clone();
        self.run_script(&ctx);
        if !self.script.is_empty() && self.script_i < self.script.len() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(msg) = self.fatal.clone() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new(tr("无法连接编程引擎", "Cannot connect to the programming engine")).size(26.0).strong().color(self.pal.err));
                    ui.add_space(10.0);
                    ui.label(RichText::new(msg).size(17.0));
                    ui.add_space(10.0);
                    ui.label(RichText::new(tr("请确认 minipro-ui.exe 与 minipro-launch.exe、minipro_winusb.dll 在同一目录，且编程器已接好。", "Make sure minipro-ui.exe, minipro-launch.exe and minipro_winusb.dll are in the same folder and the programmer is connected.")).color(self.pal.dim));
                });
            });
            return;
        }
        self.follow_system_theme(&ctx);
        self.track_zoom(&ctx);
        self.resize_handles(&ctx);
        self.watch_usb();
        ctx.request_repaint_after(Duration::from_millis(1000));
        self.top_bar(ui);
        self.footer(ui);
        self.left_panel(ui);
        self.right_panel(ui);
        self.log_panel(ui);
        self.center(ui);
        self.chip_picker(&ctx);
        self.confirm_dialog(&ctx);
        self.selfcheck_dialog(&ctx);
        self.about_window(&ctx);
        self.socket_window(&ctx);
        if !self.socket_grid.is_empty() {
            let pal = self.pal;
            egui::Area::new(egui::Id::new("socket_grid")).fixed_pos(egui::pos2(8.0, 8.0)).order(egui::Order::Foreground).show(&ctx, |ui| {
                Frame::new().fill(pal.card).inner_margin(Margin::same(6)).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        for name in &self.socket_grid {
                            ui.vertical(|ui| {
                                let pl = crate::socket::placement(name);
                                ui.label(RichText::new(name.split_whitespace().collect::<Vec<_>>().join(" ")).size(11.0));
                                ui.label(RichText::new(pl.short()).size(11.0).color(pal.dim));
                                crate::socket::draw(ui, &pal, pal.card, &pl, 310.7);
                            });
                        }
                    });
                });
            });
        }
        self.settings_window(&ctx);
        if self.busy {
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.tx.send(Job::Quit);
        std::thread::sleep(Duration::from_millis(500));
    }
}
