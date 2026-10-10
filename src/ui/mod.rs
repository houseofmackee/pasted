pub mod card;
pub mod dialogs;
pub mod icons;
pub mod preview;
pub mod shelf;
pub mod stack_panel;

use objc2::rc::Retained;
use objc2_app_kit::{NSColor, NSFont, NSLineBreakMode, NSTextField};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn argb(color: u32) -> Retained<NSColor> {
    let c = |shift: u32| ((color >> shift) & 0xff) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), c(24))
}

pub fn label(text: &str, size: f64, bold: bool, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&ns(text), mtm);
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    l.setFont(Some(&font));
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    l
}

pub fn wrapping_label(text: &str, font: &NSFont, lines: isize, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    l.setFont(Some(font));
    l.setMaximumNumberOfLines(lines);
    l.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    if let Some(cell) = l.cell() {
        cell.setTruncatesLastVisibleLine(true);
    }
    l
}
