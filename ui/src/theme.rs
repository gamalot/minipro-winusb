//! Two fixed colour schemes (light / dark) and the shared look of the widgets.

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,        // window background
    pub panel: Color32,     // side panels
    pub card: Color32,      // cards / popups
    pub stroke: Color32,    // card outlines
    pub text: Color32,
    pub dim: Color32,       // secondary text
    pub accent: Color32,
    pub accent_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub row_alt: Color32,   // hex view zebra stripe
    pub sel: Color32,       // selected byte background
    pub modified: Color32,  // edited byte text
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            dark,
            bg: Color32::from_rgb(0x0F, 0x13, 0x18),
            panel: Color32::from_rgb(0x15, 0x1A, 0x21),
            card: Color32::from_rgb(0x1B, 0x22, 0x2B),
            stroke: Color32::from_rgb(0x2A, 0x33, 0x3F),
            text: Color32::from_rgb(0xE6, 0xED, 0xF3),
            dim: Color32::from_rgb(0x8B, 0x98, 0xA6),
            accent: Color32::from_rgb(0x3B, 0x82, 0xF6),
            accent_text: Color32::WHITE,
            ok: Color32::from_rgb(0x34, 0xD3, 0x99),
            warn: Color32::from_rgb(0xFB, 0xBF, 0x24),
            err: Color32::from_rgb(0xF8, 0x71, 0x71),
            row_alt: Color32::from_rgb(0x18, 0x1E, 0x26),
            sel: Color32::from_rgb(0x2B, 0x5C, 0xB8),
            modified: Color32::from_rgb(0xFB, 0xBF, 0x24),
        }
    } else {
        Palette {
            dark,
            bg: Color32::from_rgb(0xEC, 0xEF, 0xF4),
            panel: Color32::from_rgb(0xF6, 0xF8, 0xFA),
            card: Color32::WHITE,
            stroke: Color32::from_rgb(0xD3, 0xDA, 0xE3),
            text: Color32::from_rgb(0x1B, 0x24, 0x30),
            dim: Color32::from_rgb(0x66, 0x73, 0x80),
            accent: Color32::from_rgb(0x25, 0x63, 0xEB),
            accent_text: Color32::WHITE,
            ok: Color32::from_rgb(0x05, 0x96, 0x69),
            warn: Color32::from_rgb(0xB4, 0x53, 0x09),
            err: Color32::from_rgb(0xDC, 0x26, 0x26),
            row_alt: Color32::from_rgb(0xF3, 0xF6, 0xFA),
            sel: Color32::from_rgb(0xBF, 0xD6, 0xFB),
            modified: Color32::from_rgb(0xC2, 0x41, 0x0C),
        }
    }
}

pub fn apply(ctx: &egui::Context, p: &Palette) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.stroke);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = if p.dark { Color32::from_rgb(0x10, 0x15, 0x1B) } else { Color32::WHITE };
    v.faint_bg_color = p.row_alt;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let r = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.stroke);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    for (wv, fill) in [
        (&mut w.inactive, p.card),
        (&mut w.hovered, if p.dark { Color32::from_rgb(0x26, 0x30, 0x3C) } else { Color32::from_rgb(0xE8, 0xEE, 0xF6) }),
        (&mut w.active, if p.dark { Color32::from_rgb(0x30, 0x3C, 0x4A) } else { Color32::from_rgb(0xD8, 0xE2, 0xF0) }),
    ] {
        wv.weak_bg_fill = fill;
        wv.bg_fill = fill;
        wv.corner_radius = r;
        wv.bg_stroke = Stroke::new(1.0, p.stroke);
        wv.fg_stroke = Stroke::new(1.0, p.text);
    }
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    w.noninteractive.corner_radius = r;
    w.open.corner_radius = r;
    ctx.set_visuals(v);

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 10.0);
        s.spacing.button_padding = egui::vec2(14.0, 8.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.window_margin = egui::Margin::same(16);
        s.text_styles = [
            (TextStyle::Small, FontId::new(13.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(15.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// CJK-capable fonts from Windows (the egui defaults have no Chinese glyphs).
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = std::path::Path::new(r"C:\Windows\Fonts");
    let load = |file: &str| std::fs::read(dir.join(file)).ok();
    if let Some(bytes) = load("msyh.ttc").or_else(|| load("simhei.ttf")).or_else(|| load("simsun.ttc")) {
        fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "cjk".into());
        fonts.families.entry(FontFamily::Monospace).or_default().push("cjk".into());
    }
    if let Some(bytes) = load("consola.ttf") {
        fonts.font_data.insert("mono".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    // Windows 11 icon font, used for the left tool rail (falls back to the Segoe MDL2 set)
    if let Some(bytes) = load("SegoeIcons.ttf").or_else(|| load("segmdl2.ttf")) {
        fonts.font_data.insert("icons".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        fonts.families.insert(FontFamily::Name("icons".into()), vec!["icons".into()]);
    }
    // bold face for the labels on the adapter boards of the socket diagram (falls back to the normal fonts)
    let mut bold: Vec<String> = Vec::new();
    if let Some(bytes) = load("segoeuib.ttf").or_else(|| load("arialbd.ttf")) {
        fonts.font_data.insert("bold".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        bold.push("bold".into());
    }
    bold.extend(fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
    fonts.families.insert(FontFamily::Name("bold".into()), bold);
    ctx.set_fonts(fonts);
}
