//! Hex viewer / editor: virtual scrolling, click to select, type hex digits to edit.

use crate::lang::tr;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::theme::Palette;

#[derive(Default)]
pub struct HexView {
    pub data: Vec<u8>,
    /// the state the chip / file had, to highlight what was edited
    orig: Vec<u8>,
    pub cursor: usize,
    /// true after the first of the two nibbles of a byte was typed
    half: bool,
    scroll_to_row: Option<usize>,
    pub dirty: bool,
    /// length of the search hit that starts at the cursor (0 = none); the whole hit is highlighted
    match_len: usize,
    /// scroll offset and viewport height of the last frame, to scroll only when the target row is not visible
    last_offset: f32,
    last_height: f32,
    /// number of bytes that differ from `orig`, kept up to date on every edit (not recounted every frame)
    modified: usize,
}

const ROW: f32 = 24.0;

impl HexView {
    pub fn set_data(&mut self, data: Vec<u8>) {
        self.orig = data.clone();
        self.data = data;
        self.cursor = 0;
        self.half = false;
        self.dirty = false;
        self.match_len = 0;
        self.modified = 0;
        self.scroll_to_row = Some(0);
    }

    pub fn mark_clean(&mut self) {
        self.orig = self.data.clone();
        self.dirty = false;
        self.modified = 0;
    }

    /// Recount the edited bytes (after the data was changed from outside the editor).
    pub fn recount(&mut self) {
        self.modified = self.data.iter().zip(self.orig.iter()).filter(|(a, b)| a != b).count();
    }

    fn set_byte(&mut self, i: usize, v: u8) {
        let was = self.orig.get(i).is_some_and(|o| *o != self.data[i]);
        self.data[i] = v;
        let now = self.orig.get(i).is_some_and(|o| *o != v);
        match (was, now) {
            (false, true) => self.modified += 1,
            (true, false) => self.modified = self.modified.saturating_sub(1),
            _ => {}
        }
    }

    pub fn goto(&mut self, addr: usize) {
        if self.data.is_empty() {
            return;
        }
        self.cursor = addr.min(self.data.len() - 1);
        self.half = false;
        self.match_len = 0;
        self.scroll_to_row = Some(self.cursor / 16);
    }

    pub fn find(&mut self, pat: &[u8]) -> bool {
        if pat.is_empty() || self.data.len() < pat.len() {
            return false;
        }
        let start = self.cursor + 1;
        let n = self.data.len();
        for k in 0..n {
            let i = (start + k) % n;
            if i + pat.len() <= n && self.data[i..i + pat.len()] == *pat {
                self.goto(i);
                self.match_len = pat.len();
                return true;
            }
        }
        false
    }

    pub fn modified_count(&self) -> usize {
        self.modified
    }

    fn move_cursor(&mut self, delta: i64) {
        if self.data.is_empty() {
            return;
        }
        let max = self.data.len() as i64 - 1;
        self.cursor = (self.cursor as i64 + delta).clamp(0, max) as usize;
        self.half = false;
        self.match_len = 0;
        self.scroll_to_row = Some(self.cursor / 16);
    }

    fn type_nibble(&mut self, d: u8) {
        if self.cursor >= self.data.len() {
            return;
        }
        self.match_len = 0;
        let b = self.data[self.cursor];
        if !self.half {
            self.set_byte(self.cursor, (d << 4) | (b & 0x0F));
            self.half = true;
        } else {
            self.set_byte(self.cursor, (b & 0xF0) | d);
            self.half = false;
            if self.cursor + 1 < self.data.len() {
                self.cursor += 1;
                self.scroll_to_row = Some(self.cursor / 16);
            }
        }
        self.dirty = true;
    }

    /// Width the table needs (offset column, 16 bytes, ASCII column and padding), so the card around it can hug it.
    pub fn table_width(ui: &egui::Ui) -> f32 {
        let mono = FontId::monospace(15.5);
        let cw = ui.painter().layout_no_wrap("0".into(), mono, Color32::WHITE).size().x;
        14.0 + cw * 11.0 + cw * (16.0 * 3.0 + 1.0 + 3.0) + cw * 16.0 + 14.0
    }

    /// Draw the view into the available space. `keys_enabled`: no text field has the keyboard focus.
    pub fn show(&mut self, ui: &mut egui::Ui, pal: &Palette, keys_enabled: bool) {
        let mono = FontId::monospace(15.5);
        let cw = ui.painter().layout_no_wrap("0".into(), mono.clone(), Color32::WHITE).size().x;
        let pad = 14.0;
        let hex_x = pad + cw * 11.0;
        let cell = |i: usize| hex_x + cw * (3.0 * i as f32 + if i >= 8 { 1.0 } else { 0.0 });
        let ascii_x = hex_x + cw * (16.0 * 3.0 + 1.0 + 3.0);
        let width = ascii_x + cw * 16.0 + pad;

        if self.data.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new(tr("缓冲区为空\n读取芯片或打开文件后，数据会显示在这里", "Buffer is empty\nRead the chip or open a file to see data here")).color(pal.dim).size(17.0));
            });
            return;
        }

        // ---- keyboard ---------------------------------------------------------------------------
        if keys_enabled {
            let mut digits: Vec<u8> = Vec::new();
            let (mut dx, mut dy, mut home, mut end) = (0i64, 0i64, false, false);
            ui.input(|i| {
                for ev in &i.events {
                    if let egui::Event::Text(t) = ev {
                        digits.extend(t.chars().filter_map(|c| c.to_digit(16)).map(|d| d as u8));
                    }
                }
                if i.key_pressed(Key::ArrowLeft) {
                    dx -= 1;
                }
                if i.key_pressed(Key::ArrowRight) {
                    dx += 1;
                }
                if i.key_pressed(Key::ArrowUp) {
                    dy -= 1;
                }
                if i.key_pressed(Key::ArrowDown) {
                    dy += 1;
                }
                if i.key_pressed(Key::PageUp) {
                    dy -= 24;
                }
                if i.key_pressed(Key::PageDown) {
                    dy += 24;
                }
                home = i.key_pressed(Key::Home);
                end = i.key_pressed(Key::End);
            });
            for d in digits {
                self.type_nibble(d);
            }
            if dx != 0 || dy != 0 {
                self.move_cursor(dx + dy * 16);
            }
            if home {
                self.cursor -= self.cursor % 16;
                self.half = false;
            }
            if end {
                self.cursor = (self.cursor - self.cursor % 16 + 15).min(self.data.len() - 1);
                self.half = false;
            }
        }

        // ---- header -----------------------------------------------------------------------------
        let (hrect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(width), 26.0), Sense::hover());
        let hp = ui.painter_at(hrect);
        hp.text(hrect.left_center() + Vec2::new(pad, 0.0), Align2::LEFT_CENTER, tr("偏移", "Offset"), mono.clone(), pal.dim);
        for i in 0..16 {
            hp.text(
                hrect.left_center() + Vec2::new(cell(i), 0.0),
                Align2::LEFT_CENTER,
                format!("{i:02X}"),
                mono.clone(),
                pal.dim,
            );
        }
        hp.text(hrect.left_center() + Vec2::new(ascii_x, 0.0), Align2::LEFT_CENTER, "ASCII", mono.clone(), pal.dim);
        hp.line_segment([hrect.left_bottom(), hrect.right_bottom()], Stroke::new(1.0, pal.stroke));

        // ---- rows -------------------------------------------------------------------------------
        let rows = self.data.len().div_ceil(16);
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        // rows are ROW high plus the vertical item spacing: the scroll target must use that real pitch
        let pitch = ROW + ui.spacing().item_spacing.y;
        if let Some(r) = self.scroll_to_row.take() {
            let top = r as f32 * pitch;
            let visible = top >= self.last_offset && top + ROW <= self.last_offset + self.last_height;
            if !visible || self.last_height <= 0.0 {
                // bring the row to the middle of the view
                area = area.vertical_scroll_offset((top - (self.last_height - ROW).max(0.0) / 2.0).max(0.0));
            }
        }
        let mut clicked_at: Option<usize> = None;
        let hit = self.cursor..self.cursor + self.match_len;
        let out = area.show_rows(ui, ROW, rows, |ui, range| {
            for row in range {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(width), ROW), Sense::click());
                let p = ui.painter_at(rect);
                if row % 2 == 1 {
                    p.rect_filled(rect, CornerRadius::ZERO, pal.row_alt);
                }
                let y = rect.center().y + 1.5; // Consolas glyphs sit above the line box centre: keeps text and highlight on one centre line
                p.text(egui::pos2(rect.left() + pad, y), Align2::LEFT_CENTER, format!("{:08X}", row * 16), mono.clone(), pal.dim);
                for i in 0..16 {
                    let idx = row * 16 + i;
                    if idx >= self.data.len() {
                        break;
                    }
                    let b = self.data[idx];
                    let changed = self.orig.get(idx).map(|o| *o != b).unwrap_or(false);
                    let colour = if changed {
                        pal.modified
                    } else if b == 0x00 || b == 0xFF {
                        pal.dim.gamma_multiply(0.7)
                    } else {
                        pal.text
                    };
                    let hx = rect.left() + cell(i);
                    let ax = rect.left() + ascii_x + cw * i as f32;
                    if hit.contains(&idx) && hit.len() > 1 {
                        // one continuous band over the hit in both columns
                        let last = idx + 1 == hit.end || i == 15;
                        let w = if last { cw * 2.0 + 6.0 } else { cw * 3.0 + 1.0 };
                        let r1 = Rect::from_min_size(egui::pos2(hx - 3.0, rect.top() + 0.5), Vec2::new(w, ROW - 4.0));
                        let r2 = Rect::from_min_size(egui::pos2(ax - 1.0, rect.top() + 0.5), Vec2::new(cw + 2.0, ROW - 4.0));
                        for r in [r1, r2] {
                            p.rect_filled(r, CornerRadius::same(3), pal.accent.gamma_multiply(0.38));
                        }
                    }
                    if idx == self.cursor {
                        let r1 = Rect::from_min_size(egui::pos2(hx - 3.0, rect.top() + 0.5), Vec2::new(cw * 2.0 + 6.0, ROW - 4.0));
                        let r2 = Rect::from_min_size(egui::pos2(ax - 1.0, rect.top() + 0.5), Vec2::new(cw + 2.0, ROW - 4.0));
                        for r in [r1, r2] {
                            p.rect_filled(r, CornerRadius::same(4), pal.sel);
                            p.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, pal.accent), StrokeKind::Inside);
                        }
                    }
                    p.text(egui::pos2(hx, y), Align2::LEFT_CENTER, format!("{b:02X}"), mono.clone(), colour);
                    let ch = if (0x20..0x7F).contains(&b) { b as char } else { '·' };
                    let ac = if (0x20..0x7F).contains(&b) { colour } else { pal.dim.gamma_multiply(0.6) };
                    p.text(egui::pos2(ax, y), Align2::LEFT_CENTER, ch.to_string(), mono.clone(), ac);
                }
                if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        let x = pos.x - rect.left();
                        let col = if x >= ascii_x - cw {
                            (((x - ascii_x) / cw).floor() as i64).clamp(0, 15) as usize
                        } else {
                            let mut best = 0usize;
                            let mut bd = f32::MAX;
                            for i in 0..16 {
                                let d = (x - (cell(i) + cw)).abs();
                                if d < bd {
                                    bd = d;
                                    best = i;
                                }
                            }
                            best
                        };
                        clicked_at = Some((row * 16 + col).min(self.data.len() - 1));
                    }
                }
            }
        });
        self.last_offset = out.state.offset().y;
        self.last_height = out.inner_rect.height();
        if let Some(i) = clicked_at {
            self.cursor = i;
            self.half = false;
            self.match_len = 0;
        }
    }
}
