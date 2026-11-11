//! UI language (English by default, Chinese switchable in the settings menu).
//!
//! `tr("中文", "English")` picks a plain string, `trf!("中文 {}", "English {}", args…)` a formatted one.
//! The choice is a process-wide flag so the engine thread can use it too.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(true);

pub fn is_en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

pub fn set_en(en: bool) {
    ENGLISH.store(en, Ordering::Relaxed);
}

pub fn tr(zh: &'static str, en: &'static str) -> &'static str {
    if is_en() {
        en
    } else {
        zh
    }
}

macro_rules! trf {
    ($zh:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::lang::is_en() { format!($en $(, $arg)*) } else { format!($zh $(, $arg)*) }
    };
}
pub(crate) use trf;
