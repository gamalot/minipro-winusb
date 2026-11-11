//! How the selected chip goes into the programmer's ZIF socket, drawn by this program in the theme's colours.
//!
//! The method follows minipro-rs (https://gitlab.com/arcturus8081/minipro-rs): the TL866CS has a 40-pin socket
//! with the lever at the pin-1 end and chips are top-justified; pin count, PLCC adapter, special adapter and ICSP
//! come from the `package_details` word of the chip database.  `assets/packages.tsv` is that word for every chip
//! of minipro's `infoic.xml` (database INFOIC): one line per `package_details` value, followed by the chip names
//! (upper case, without spaces) that use it.
//!
//! Which picture belongs to which chip, and its proportions, were checked against the pictures the official
//! program draws ("Location In Socket"):
//!   * no `@package` in the name, or `@DIPnn`  -> the bare DIP chip
//!   * any other `@package` with a pin count   -> the green "SO-DIPnn" adapter board
//!   * pin byte 0x3f / 0x3d                    -> the PLCC32/DIP32 and PLCC44/DIP40 (MCS51) adapters
//!   * adapter byte 1..9                       -> no picture, "Use Adapter=>NAME"
//!   * no pin count, ICSP byte set             -> no picture, connect the ICSP cable
//! All coordinates below are pixels of the official 129 x 291 picture, scaled to the requested height.

use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{self, text::LayoutJob, Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Sense, Shape, Stroke, StrokeKind, TextFormat, Vec2};

use crate::lang::{tr, trf};
use crate::theme::Palette;

#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// DIP chip straight in the socket
    Dip { pins: u8 },
    /// small-outline package on the generic SO-DIP adapter board
    So { pins: u8 },
    /// PLCC adapter that plugs in like a DIP of `dip` pins
    Plcc { pins: u8, dip: u8, family: &'static str },
    /// a dedicated adapter the official program has no picture for
    Adapter(&'static str),
    /// programmed through the ICSP connector, the socket is not used
    Icsp,
    Unknown,
}

fn table() -> &'static HashMap<&'static str, u32> {
    static T: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        for line in include_str!("../assets/packages.tsv").lines() {
            let mut it = line.split('\t');
            let Some(v) = it.next().and_then(|h| u32::from_str_radix(h, 16).ok()) else { continue };
            for name in it {
                m.insert(name, v);
            }
        }
        m
    })
}

fn adapter_name(id: u32) -> Option<&'static str> {
    // names as the official program prints them ("Use Adapter=>...")
    Some(match id {
        1 => "TSOP48",
        2 => "SOP44",
        3 => "TSOP40A-8bits",
        4 => "TSOP40B-16bits",
        5 => "TSOP32",
        6 => "SOP56 SV29802",
        7 => "TQFP64 DIY",
        8 => "25 PIN8_1.8V",
        9 => "25 PIN16_1.8V",
        _ => return None,
    })
}

/// Placement of a chip, from its name as the official program shows it (e.g. `SST39SF020A @PLCC32`).
pub fn placement(chip: &str) -> Placement {
    let key: String = chip.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_uppercase();
    let suffix = chip.split_once('@').map(|(_, s)| s.trim().to_uppercase());
    let on_board = suffix.as_deref().is_some_and(|s| !s.starts_with("DIP"));
    let by_pins = |pins: u32| -> Placement {
        if pins == 0 || pins % 2 != 0 || pins > 40 {
            Placement::Unknown
        } else if on_board {
            Placement::So { pins: pins as u8 }
        } else {
            Placement::Dip { pins: pins as u8 }
        }
    };
    match table().get(key.as_str()) {
        Some(&raw) => {
            let (pin, icsp, adapter) = ((raw >> 24) & 0x3f, (raw >> 8) & 0xff, raw & 0xff);
            if adapter != 0 {
                adapter_name(adapter).map(Placement::Adapter).unwrap_or(Placement::Unknown)
            } else {
                match pin {
                    0x3f => Placement::Plcc { pins: 32, dip: 32, family: "" },
                    0x3d => Placement::Plcc { pins: 44, dip: 40, family: "MCS51" },
                    0x3e => Placement::Plcc { pins: 28, dip: 28, family: "" },
                    0x38 => Placement::Plcc { pins: 20, dip: 20, family: "" },
                    0 if icsp != 0 => Placement::Icsp,
                    n => by_pins(n),
                }
            }
        }
        // not in the table: only what the name itself says
        None => match suffix.as_deref() {
            Some(s) if s.starts_with("ICSP") || s.starts_with("ICP") => Placement::Icsp,
            Some(s) => {
                let word = s.split_whitespace().next().unwrap_or("");
                let family = word.trim_end_matches(|c: char| c.is_ascii_digit());
                let digits = &word[family.len()..];
                match digits.parse::<u32>() {
                    Ok(n) if matches!(family, "DIP" | "SOIC" | "SOP" | "SOP-J" | "TSSOP" | "MSOP" | "SSOP" | "DFN" | "WSON" | "USON" | "SON" | "VDFN") => by_pins(n),
                    _ => Placement::Unknown,
                }
            }
            None => Placement::Unknown,
        },
    }
}

impl Placement {
    pub fn has_diagram(&self) -> bool {
        matches!(self, Placement::Dip { .. } | Placement::So { .. } | Placement::Plcc { .. })
    }

    /// A few words for the chip card.
    pub fn short(&self) -> String {
        match self {
            Placement::Dip { pins } => format!("DIP{pins}"),
            Placement::So { pins } => trf!("SO-DIP{} 转接板", "SO-DIP{} adapter", pins),
            Placement::Plcc { pins, dip, .. } => trf!("PLCC{} 转 DIP{}", "PLCC{} to DIP{}", pins, dip),
            Placement::Adapter(name) => trf!("转接座 {}", "Adapter {}", name),
            Placement::Icsp => "ICSP".into(),
            Placement::Unknown => String::new(),
        }
    }

    /// One or two sentences: what to do.
    pub fn describe(&self) -> String {
        match self {
            Placement::Dip { pins } => trf!(
                "DIP{} 直接插入插座，靠手柄一端对齐，1 脚朝手柄；放好后压下手柄锁紧。",
                "Insert the DIP{} chip directly, pushed up against the lever end with pin 1 towards the lever, then lock the lever.",
                pins
            ),
            Placement::So { pins } => trf!(
                "芯片装在 SO-DIP{} 转接板上，转接板靠手柄一端对齐，板上的 1# 朝手柄；放好后压下手柄锁紧。",
                "Mount the chip on the SO-DIP{} adapter board; the board sits against the lever end with its 1# mark towards the lever. Then lock the lever.",
                pins
            ),
            Placement::Plcc { pins, dip, .. } => trf!(
                "芯片装进 PLCC{} 转 DIP{} 转接座，转接座靠手柄一端对齐，1# 朝手柄；放好后压下手柄锁紧。",
                "Put the chip in the PLCC{} to DIP{} adapter; the adapter sits against the lever end with its 1# mark towards the lever. Then lock the lever.",
                pins,
                dip
            ),
            Placement::Adapter(name) => trf!(
                "这个芯片需要专用转接座：{}。把转接座插入 40 脚插座并锁紧（原厂程序对这种转接座也没有示意图）。",
                "This chip needs the dedicated adapter {}. Plug the adapter into the 40-pin socket and lock the lever (the official program has no diagram for it either).",
                name
            ),
            Placement::Icsp => tr("这个芯片通过 ICSP 接口连接目标板来编程，不使用插座。", "This chip is programmed through the ICSP connector wired to the target board; the socket is not used.").to_string(),
            Placement::Unknown => tr("没有这个芯片的封装信息。", "No package information for this chip.").to_string(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// drawing
// ---------------------------------------------------------------------------------------------

/// part of the official picture that is drawn: x 6..123, y 29..262
const X0: f32 = 6.0;
const Y0: f32 = 29.0;
const W: f32 = 117.0;
const H: f32 = 233.0;

struct Colours {
    frame: Color32,
    slot: Color32,
    body: Color32,
    body_edge: Color32,
    pin: Color32,
    dot: Color32,
    board: Color32,
    board_edge: Color32,
    pad: Color32,
    pad_hole: Color32,
    so_body: Color32,
    label: Color32,
}

fn colours(pal: &Palette) -> Colours {
    if pal.dark {
        Colours {
            frame: Color32::from_rgb(0x8a, 0x94, 0xa3),
            slot: pal.accent.gamma_multiply(0.85),
            body: Color32::from_rgb(0x56, 0x60, 0x6e),
            body_edge: Color32::from_rgb(0x84, 0x8f, 0x9f),
            pin: Color32::from_rgb(0x84, 0x8f, 0x9f),
            dot: Color32::from_rgb(0xc4, 0xcb, 0xd6),
            board: Color32::from_rgb(0x2f, 0x86, 0x4c),
            board_edge: Color32::from_rgb(0x66, 0xc0, 0x84),
            pad: Color32::from_rgb(0xd8, 0xcf, 0x7e),
            pad_hole: Color32::from_rgb(0x9a, 0xa0, 0xa8),
            so_body: Color32::from_rgb(0x2a, 0x30, 0x39),
            label: Color32::from_rgb(0xf2, 0xf6, 0xf2),
        }
    } else {
        // the official colours
        Colours {
            frame: Color32::from_rgb(0x64, 0x64, 0x64),
            slot: Color32::from_rgb(0x50, 0x50, 0xbe),
            body: Color32::from_rgb(0x6e, 0x6e, 0x6e),
            body_edge: Color32::from_rgb(0x96, 0x96, 0x96),
            pin: Color32::from_rgb(0x78, 0x78, 0x78),
            dot: Color32::from_rgb(0xbe, 0xbe, 0xbe),
            board: Color32::from_rgb(0x7c, 0xcd, 0x7c),
            board_edge: Color32::from_rgb(0x5a, 0x5a, 0x5a),
            pad: Color32::from_rgb(0xf0, 0xe6, 0x8c),
            pad_hole: Color32::from_rgb(0xbe, 0xbe, 0xbe),
            so_body: Color32::from_rgb(0x50, 0x50, 0x50),
            label: Color32::from_rgb(0xfa, 0xfa, 0xfa),
        }
    }
}

/// Draws the socket with the chip or adapter in place, `height` points high.  `bg` is the colour of the
/// surface underneath (it shows through the notch of a DIP chip).
pub fn draw(ui: &mut egui::Ui, pal: &Palette, bg: Color32, pl: &Placement, height: f32) {
    let s = height / H;
    let (area, _) = ui.allocate_exact_size(Vec2::new(W * s, height), Sense::hover());
    let painter = ui.painter_at(area);
    let c = colours(pal);
    let p = |x: f32, y: f32| egui::pos2(area.min.x + (x - X0) * s, area.min.y + (y - Y0) * s);
    let rc = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(p(x0, y0), p(x1, y1));
    let fill = |r: Rect, col: Color32| painter.rect_filled(r, CornerRadius::ZERO, col);
    let text = |pos: egui::Pos2, anchor: Align2, t: &str, size: f32, col: Color32| {
        let mut job = LayoutJob::default();
        job.append(t, 0.0, TextFormat { font_id: FontId::new(size * s, FontFamily::Name("bold".into())), color: col, italics: true, ..Default::default() });
        let galley = painter.layout_job(job);
        let r = anchor.anchor_size(pos, galley.size());
        painter.galley(r.min, galley, col);
    };
    let row_y = |row: u8| 58.0 + 10.0 * row as f32;

    // lever: knob ring and stem at the pin-1 end
    painter.rect_stroke(rc(17.5, 32.5, 27.5, 48.5), CornerRadius::same((4.0 * s) as u8), Stroke::new(3.0 * s, c.frame), StrokeKind::Middle);
    fill(rc(21.0, 49.0, 24.0, 53.0), c.frame);
    // socket body
    painter.rect_stroke(rc(19.5, 53.5, 109.5, 258.5), CornerRadius::ZERO, Stroke::new(3.0 * s, c.frame), StrokeKind::Middle);
    // 20 rows of contact slots
    let line = Stroke::new((1.0 * s).max(1.0), c.slot);
    for row in 0..20 {
        let y = row_y(row);
        painter.rect_stroke(rc(27.0, y, 58.0, y + 6.0), CornerRadius::ZERO, line, StrokeKind::Inside);
        painter.rect_stroke(rc(72.0, y, 103.0, y + 6.0), CornerRadius::ZERO, line, StrokeKind::Inside);
    }

    match *pl {
        Placement::Dip { pins } => {
            let rows = pins / 2;
            let bottom = row_y(rows - 1) + 6.0;
            for row in 0..rows {
                let y = row_y(row) + 2.0;
                fill(rc(38.0, y, 43.0, y + 2.0), c.pin);
                fill(rc(85.0, y, 91.0, y + 2.0), c.pin);
            }
            fill(rc(43.0, 57.0, 85.0, bottom), c.body_edge);
            fill(rc(45.0, 59.0, 83.0, bottom - 2.0), c.body);
            // notch at the pin-1 end, pin-1 dot
            fill(rc(59.0, 57.0, 70.0, 64.0), c.body_edge);
            fill(rc(61.0, 57.0, 68.0, 62.0), bg);
            painter.circle_filled(p(52.0, 68.0), 4.0 * s, c.dot);
        }
        Placement::So { pins } => {
            let rows = pins / 2;
            let board_bottom = board(&painter, &c, &rc, &p, s, rows);
            // the chip in the middle of the board
            let n = rows as f32;
            let ch = n * 5.0 + 5.0;
            let top = ((56.0 + board_bottom) / 2.0 - ch / 2.0 - 1.5).round();
            for i in 0..rows {
                let y = top + 4.0 + 5.0 * i as f32;
                fill(rc(47.0, y, 52.0, y + 2.0), c.pin);
                fill(rc(78.0, y, 84.0, y + 2.0), c.pin);
            }
            fill(rc(52.0, top, 78.0, top + ch), c.pin);
            fill(rc(54.0, top + 2.0, 76.0, top + ch - 2.0), c.so_body);
            let r = (3.0 * s) as u8;
            painter.rect_filled(rc(61.0, top, 69.0, top + 6.0), CornerRadius { nw: 0, ne: 0, sw: r, se: r }, c.pin);
            painter.rect_filled(rc(63.0, top, 68.0, top + 4.0), CornerRadius { nw: 0, ne: 0, sw: r, se: r }, c.board);
            painter.circle_filled(p(57.0, top + 6.0), 2.0 * s, c.dot);
            if pins > 8 {
                text(p(65.5, board_bottom - 9.5), Align2::CENTER_CENTER, &format!("SO-DIP{pins}"), 10.5, c.label);
                text(p(45.0, 57.5), Align2::LEFT_TOP, "1#", 10.5, c.label);
            } else {
                // the 8-pin board has no room: the mark goes next to the socket
                text(p(16.0, 57.0), Align2::RIGHT_TOP, "1#", 10.5, pal.text);
            }
        }
        Placement::Plcc { pins, dip, family } => {
            let rows = dip / 2;
            let board_bottom = board(&painter, &c, &rc, &p, s, rows);
            let top = ((56.0 + board_bottom) / 2.0 - 38.0).round();
            // square package with the cut corner at pin 1
            painter.add(Shape::convex_polygon(
                vec![p(41.0, top), p(97.0, top), p(97.0, top + 72.0), p(33.0, top + 72.0), p(33.0, top + 8.0)],
                c.body_edge,
                Stroke::NONE,
            ));
            painter.add(Shape::convex_polygon(
                vec![p(43.0, top + 4.0), p(93.0, top + 4.0), p(93.0, top + 68.0), p(37.0, top + 68.0), p(37.0, top + 10.0)],
                c.body,
                Stroke::NONE,
            ));
            painter.line_segment([p(39.0, top + 11.5), p(91.0, top + 11.5)], Stroke::new((1.0 * s).max(1.0), c.body_edge.gamma_multiply(0.55)));
            painter.circle_filled(p(65.0, top + 7.0), 2.0 * s, c.dot);
            text(p(45.0, 57.5), Align2::LEFT_TOP, "1#", 10.5, c.label);
            if !family.is_empty() {
                text(p(65.5, (56.0 + top) / 2.0 + 1.0), Align2::CENTER_CENTER, family, 10.5, c.label);
            }
            let mid = (top + 72.0 + board_bottom) / 2.0;
            text(p(65.5, mid - 7.5), Align2::CENTER_CENTER, &format!("PLCC{pins}"), 10.5, c.label);
            text(p(65.5, mid + 7.5), Align2::CENTER_CENTER, &format!("DIP{dip}"), 10.5, c.label);
        }
        _ => {}
    }
}

/// Green adapter board over the first `rows` rows, one pad per row and side.  Returns the y of its lower edge.
fn board(painter: &egui::Painter, c: &Colours, rc: &dyn Fn(f32, f32, f32, f32) -> Rect, p: &dyn Fn(f32, f32) -> egui::Pos2, s: f32, rows: u8) -> f32 {
    let bottom = 58.0 + 10.0 * (rows - 1) as f32 + 8.0;
    let r = rc(32.0, 56.0, 99.0, bottom);
    painter.rect_filled(r, CornerRadius::ZERO, c.board);
    painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new((1.0 * s).max(1.0), c.board_edge), StrokeKind::Inside);
    let diamond = |cx: f32, cy: f32, rad: f32, col: Color32| {
        painter.add(Shape::convex_polygon(vec![p(cx, cy - rad), p(cx + rad, cy), p(cx, cy + rad), p(cx - rad, cy)], col, Stroke::NONE));
    };
    for row in 0..rows {
        let cy = 58.0 + 10.0 * row as f32 + 3.0;
        for cx in [39.0, 93.0] {
            diamond(cx, cy, 3.2, c.pad);
            diamond(cx, cy, 1.9, c.pad_hole);
        }
    }
    bottom
}
