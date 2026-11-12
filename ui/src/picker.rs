//! Chip picker, laid out like the official "Select Device" dialog: keyword search, chip type, manufacturer list
//! and device list.  Click highlights a device; double-click, Enter or "Select" chooses it.

use std::collections::HashMap;

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Vec2};

use crate::app::{modal, segmented, with_commas};
use crate::engine::ChipEntry;
use crate::lang::{tr, trf};
use crate::theme::Palette;

/// Chip types of the official database, in the order of its type buttons; type `i` is bit `i` of
/// [`ChipEntry::cats`].
pub const CATS: [(&str, &str); 5] = [
    ("ROM/闪存/NVRAM", "ROM/Flash/NVRAM"),
    ("单片机 MCU/MPU", "MCU/MPU"),
    ("PLD/GAL/CPLD", "PLD/GAL/CPLD"),
    ("SRAM/DRAM", "SRAM/DRAM"),
    ("逻辑芯片", "Logic IC"),
];

/// The official names are padded with spaces ("AT89C1051    "): show them with single spaces.
pub fn display_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

const MFR_ROW: f32 = 32.0;
const CHIP_ROW: f32 = 34.0;
const LIST_H: f32 = 400.0;

pub enum Outcome {
    Open,
    Closed,
    Chosen(usize),
}

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    pub search: String,
    /// chip type filter (index into [`CATS`]); None = all types
    pub cat: Option<usize>,
    /// manufacturer filter; None = all manufacturers
    pub mfr: Option<String>,
    /// highlighted chip (index into the chip list)
    pub sel: Option<usize>,
    scroll_chip: bool,
    scroll_mfr: bool,
    /// filter the cached lists below were made for: (search, type, manufacturer, number of chips)
    made_for: Option<(String, Option<usize>, Option<String>, usize)>,
    /// manufacturers that have chips under the search and type filter, in database order, with their count
    mfr_rows: Vec<(String, usize)>,
    /// chips under the search and type filter, all manufacturers
    total: usize,
    /// chips shown: search, type and manufacturer filter
    rows: Vec<usize>,
}

impl Picker {
    /// Opens the picker on the current chip: its manufacturer selected and the chip highlighted, like the
    /// official dialog.
    pub fn open_at(&mut self, chips: &[ChipEntry], current: &str) {
        self.open = true;
        self.search.clear();
        self.cat = None;
        self.made_for = None;
        let cur = display_name(current);
        match chips.iter().position(|c| !cur.is_empty() && display_name(&c.name) == cur) {
            Some(i) => {
                self.mfr = Some(chips[i].mfr.clone());
                self.sel = Some(i);
            }
            None => {
                self.mfr = None;
                self.sel = None;
            }
        }
        self.scroll_chip = true;
        self.scroll_mfr = true;
    }

    fn refilter(&mut self, chips: &[ChipEntry]) {
        let key = (self.search.clone(), self.cat, self.mfr.clone(), chips.len());
        if self.made_for.as_ref() == Some(&key) {
            return;
        }
        let tokens: Vec<String> = self.search.to_lowercase().split_whitespace().map(String::from).collect();
        let mut matching: Vec<(u8, usize)> = Vec::new();
        let mut mfr_rows: Vec<(String, usize)> = Vec::new();
        let mut mfr_at: HashMap<&str, usize> = HashMap::new();
        for (i, c) in chips.iter().enumerate() {
            if let Some(b) = self.cat {
                if c.cats & (1 << b) == 0 {
                    continue;
                }
            }
            let mut rank = 0;
            if !tokens.is_empty() {
                let name = c.name.to_lowercase();
                let hay = format!("{name} {}", c.mfr.to_lowercase());
                if !tokens.iter().all(|t| hay.contains(t.as_str())) {
                    continue;
                }
                // models starting with the first word come first
                rank = if name.starts_with(tokens[0].as_str()) { 0 } else { 1 };
            }
            matching.push((rank, i));
            match mfr_at.get(c.mfr.as_str()) {
                Some(&k) => mfr_rows[k].1 += 1,
                None => {
                    mfr_at.insert(&c.mfr, mfr_rows.len());
                    mfr_rows.push((c.mfr.clone(), 1));
                }
            }
        }
        // a manufacturer with nothing left under the other filters stops filtering
        if self.mfr.as_ref().is_some_and(|m| !mfr_at.contains_key(m.as_str())) {
            self.mfr = None;
        }
        matching.sort_by_key(|&(r, i)| (r, i));
        self.total = matching.len();
        self.rows = matching.into_iter().map(|(_, i)| i).filter(|&i| self.mfr.as_ref().is_none_or(|m| chips[i].mfr == *m)).collect();
        self.mfr_rows = mfr_rows;
        if self.sel.is_some_and(|s| !self.rows.contains(&s)) {
            self.sel = None;
        }
        // a new filter: show the highlighted chip (or the top) and the selected manufacturer
        self.scroll_chip = true;
        self.scroll_mfr = true;
        self.made_for = Some((self.search.clone(), self.cat, self.mfr.clone(), chips.len()));
    }

    pub fn show(&mut self, ctx: &egui::Context, pal: &Palette, chips: &[ChipEntry], full_names: &HashMap<String, String>, building: bool) -> Outcome {
        if !self.open {
            return Outcome::Closed;
        }
        let mut chosen: Option<usize> = None;
        let mut cancel = false;
        let close = modal(ctx, pal, "chip-picker", tr("选择芯片", "Choose chip"), 900.0, |ui| {
            if chips.is_empty() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    let msg = if building {
                        tr("正在建立芯片库，首次使用需要约一分钟…", "Building the chip database, the first run takes about a minute…")
                    } else {
                        tr("芯片库尚未建立", "Chip database not built yet")
                    };
                    ui.label(RichText::new(msg).color(pal.dim).size(17.0));
                });
                ui.add_space(30.0);
                return;
            }
            self.refilter(chips);

            // ---- search ----------------------------------------------------------------------------
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::symmetric(12, 9))
                    .hint_text(tr("搜索型号或厂商，例如 SST39SF020A、W25Q64、24C02、ATMEGA328", "Search model or manufacturer, e.g. SST39SF020A, W25Q64, 24C02, ATMEGA328"))
                    .font(egui::FontId::proportional(17.0)),
            );
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            if r.changed() {
                self.refilter(chips);
            }
            ui.add_space(8.0);

            // ---- type ------------------------------------------------------------------------------
            let mut items: Vec<(&str, bool)> = vec![(tr("全部类型", "All types"), self.cat.is_none())];
            items.extend(CATS.iter().enumerate().map(|(i, (zh, en))| (tr(zh, en), self.cat == Some(i))));
            if let Some(k) = segmented(ui, pal, ui.available_width(), &items) {
                self.cat = if k == 0 { None } else { Some(k - 1) };
                self.refilter(chips);
            }
            ui.add_space(10.0);

            // ---- keyboard: arrows move the highlight, Enter chooses ---------------------------------
            let (up, down, enter) = ui.input(|i| (i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::Enter)));
            if (up || down) && !self.rows.is_empty() {
                let at = self.sel.and_then(|s| self.rows.iter().position(|&r| r == s));
                let next = match (at, down) {
                    (None, _) => 0,
                    (Some(k), true) => (k + 1).min(self.rows.len() - 1),
                    (Some(k), false) => k.saturating_sub(1),
                };
                self.sel = Some(self.rows[next]);
                self.scroll_chip = true;
            }
            if enter {
                chosen = self.sel.or(self.rows.first().copied());
            }

            // ---- manufacturer and device lists -------------------------------------------------------
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let mfr_w = 290.0;
                pane(ui, pal, mfr_w, tr("厂商", "Manufacturer"), &with_commas(self.mfr_rows.len() as u64), |ui| {
                    let n = self.mfr_rows.len() + 1;
                    ui.spacing_mut().item_spacing.y = 0.0; // show_rows pitch = row height (scroll targets use it)
                    let mut area = egui::ScrollArea::vertical().id_salt("picker-mfrs").max_height(LIST_H).min_scrolled_height(LIST_H).auto_shrink([false, false]);
                    if std::mem::take(&mut self.scroll_mfr) {
                        let k = self.mfr.as_ref().and_then(|m| self.mfr_rows.iter().position(|(x, _)| x == m)).map(|k| k + 1).unwrap_or(0);
                        area = area.vertical_scroll_offset((k as f32 * MFR_ROW - LIST_H / 2.0 + MFR_ROW / 2.0).max(0.0));
                    }
                    area.show_rows(ui, MFR_ROW, n, |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for k in range {
                            let (label, count, on) = if k == 0 {
                                (tr("全部厂商", "All manufacturers").to_string(), self.total, self.mfr.is_none())
                            } else {
                                let (m, c) = &self.mfr_rows[k - 1];
                                (m.clone(), *c, self.mfr.as_deref() == Some(m.as_str()))
                            };
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), MFR_ROW), Sense::click());
                            row_bg(ui, pal, rect, on, resp.hovered(), false);
                            let (fg, dim) = if on { (pal.accent_text, pal.accent_text) } else { (pal.text, pal.dim) };
                            let p = ui.painter_at(rect);
                            p.text(rect.left_center() + Vec2::new(10.0, 0.0), Align2::LEFT_CENTER, &label, egui::FontId::proportional(14.5), fg);
                            p.text(rect.right_center() - Vec2::new(10.0, 0.0), Align2::RIGHT_CENTER, with_commas(count as u64), egui::FontId::proportional(13.0), dim);
                            if resp.clicked() {
                                self.mfr = if k == 0 { None } else { Some(label) };
                                self.refilter(chips);
                                self.scroll_mfr = false; // the clicked row is in view already
                            }
                        }
                    });
                });
                let chip_w = ui.available_width();
                let count = if self.rows.len() == chips.len() {
                    with_commas(chips.len() as u64)
                } else {
                    trf!("{} / {}", "{} of {}", with_commas(self.rows.len() as u64), with_commas(chips.len() as u64))
                };
                pane(ui, pal, chip_w, tr("型号", "Device"), &count, |ui| {
                    let mut area = egui::ScrollArea::vertical().id_salt("picker-chips").max_height(LIST_H).min_scrolled_height(LIST_H).auto_shrink([false, false]);
                    if std::mem::take(&mut self.scroll_chip) {
                        let k = self.sel.and_then(|s| self.rows.iter().position(|&r| r == s));
                        let off = k.map(|k| k as f32 * CHIP_ROW - LIST_H / 2.0 + CHIP_ROW / 2.0).unwrap_or(0.0);
                        area = area.vertical_scroll_offset(off.max(0.0));
                    }
                    ui.spacing_mut().item_spacing.y = 0.0; // show_rows pitch = row height (scroll targets use it)
                    if self.rows.is_empty() {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(tr("没有匹配的型号", "No matching device")).color(pal.dim).size(15.0));
                        });
                        ui.add_space(LIST_H - 60.0);
                        return;
                    }
                    let all_mfrs = self.mfr.is_none();
                    area.show_rows(ui, CHIP_ROW, self.rows.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for k in range {
                            let i = self.rows[k];
                            let c = &chips[i];
                            let on = self.sel == Some(i);
                            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), CHIP_ROW), Sense::click());
                            row_bg(ui, pal, rect, on, resp.hovered(), k % 2 == 1);
                            let (fg, dim) = if on { (pal.accent_text, pal.accent_text) } else { (pal.text, pal.dim) };
                            let p = ui.painter_at(rect);
                            p.text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, display_name(&c.name), egui::FontId::proportional(16.0), fg);
                            if all_mfrs {
                                p.text(rect.right_center() - Vec2::new(12.0, 0.0), Align2::RIGHT_CENTER, &c.mfr, egui::FontId::proportional(13.0), dim);
                            }
                            if resp.clicked() {
                                self.sel = Some(i);
                            }
                            if resp.double_clicked() {
                                chosen = Some(i);
                            }
                        }
                    });
                });
            });

            // ---- footer: the highlighted chip, Cancel / Select -------------------------------------
            ui.add_space(16.0);
            ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 44.0), Layout::left_to_right(Align::Center), |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    match self.sel.map(|i| &chips[i]) {
                        Some(c) => {
                            ui.label(RichText::new(display_name(&c.name)).size(16.0).strong().color(pal.text));
                            let full = full_names.get(&c.mfr).filter(|f| !f.is_empty() && **f != c.mfr);
                            let who = match full {
                                Some(f) => format!("{} · {f}", c.mfr),
                                None => c.mfr.clone(),
                            };
                            ui.label(RichText::new(who).size(13.0).color(pal.dim));
                        }
                        None => {
                            ui.label(RichText::new(tr("单击选中型号，双击或按回车确定", "Click a device to highlight it; double-click or press Enter to choose it")).size(13.5).color(pal.dim));
                        }
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let b = egui::Button::new(RichText::new(tr("选择", "Select")).size(15.0).strong().color(Color32::WHITE))
                        .fill(pal.accent)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(9))
                        .min_size(Vec2::new(130.0, 40.0));
                    if ui.add_enabled(self.sel.is_some(), b).clicked() {
                        chosen = self.sel;
                    }
                    let c = egui::Button::new(RichText::new(tr("取消", "Cancel")).size(15.0).color(pal.text))
                        .corner_radius(CornerRadius::same(9))
                        .min_size(Vec2::new(104.0, 40.0));
                    if ui.add(c).clicked() {
                        cancel = true;
                    }
                });
            });
        });
        if let Some(i) = chosen {
            self.open = false;
            return Outcome::Chosen(i);
        }
        if close || cancel {
            self.open = false;
            return Outcome::Closed;
        }
        Outcome::Open
    }
}

/// A titled, framed list area of fixed width.
fn pane(ui: &mut egui::Ui, pal: &Palette, width: f32, title: &str, count: &str, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(pal.bg)
        .stroke(Stroke::new(1.0, pal.stroke))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(6))
        .show(ui, |ui| {
            // the frame inherits the horizontal layout of the two panes: stack header and list explicitly
            ui.vertical(|ui| {
                ui.set_width(width - 14.0);
                ui.allocate_ui_with_layout(Vec2::new(ui.available_width(), 26.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.add_space(6.0);
                    ui.label(RichText::new(title).size(13.5).strong().color(pal.dim));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(6.0);
                        ui.label(RichText::new(count).size(13.0).color(pal.dim));
                    });
                });
                add(ui);
            });
        });
}

fn row_bg(ui: &egui::Ui, pal: &Palette, rect: egui::Rect, on: bool, hovered: bool, alt: bool) {
    let p = ui.painter();
    if on {
        p.rect_filled(rect, CornerRadius::same(7), pal.accent);
    } else if hovered {
        p.rect_filled(rect, CornerRadius::same(7), pal.accent.gamma_multiply(0.22));
    } else if alt {
        p.rect_filled(rect, CornerRadius::same(7), pal.row_alt);
    }
}
