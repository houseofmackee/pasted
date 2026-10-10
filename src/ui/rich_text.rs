//! Attributed strings for rich text clips, from RTF (AppKit's fast RTF
//! reader) or HTML (our own converter in `crate::rich`).
//!
//! Source documents assume their own background, so fonts are scaled to fit,
//! background highlights are dropped, and text colors that would be hard to
//! read on the card are replaced with the system label color.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAttachmentAttributeName, NSAttributedStringAppKitDocumentFormats, NSBackgroundColorAttributeName, NSColor,
    NSColorSpace, NSFont, NSFontAttributeName, NSFontManager, NSFontTraitMask, NSFontWeightBold, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSLineBreakMode, NSLinkAttributeName, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSStrikethroughStyleAttributeName, NSUnderlineStyleAttributeName,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSData, NSMutableAttributedString, NSNumber, NSRange,
    NSString,
};

use super::icons::relative_luminance;
use crate::model::{Item, ItemKind, UTI_HTML, UTI_RTF};
use crate::rich::{Run, html_to_runs};
use crate::store::Store;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Card,
    Preview,
}

impl Target {
    fn max_chars(self) -> usize {
        match self {
            Target::Card => 1200,
            Target::Preview => 60_000,
        }
    }

    fn base_size(self) -> f64 {
        match self {
            Target::Card => 12.5,
            Target::Preview => 14.0,
        }
    }

    fn clamp_size(self, size: f64) -> f64 {
        match self {
            Target::Card => (size * 0.9).clamp(9.0, 16.0),
            Target::Preview => size.clamp(10.0, 28.0),
        }
    }
}

fn srgb(color: &NSColor) -> Option<[f64; 3]> {
    let c = color.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
    // SAFETY: valid out pointers.
    unsafe { c.getRed_green_blue_alpha(&mut r, &mut g, &mut b, &mut a) };
    Some([r, g, b])
}

/// True when `fg` has at least 3:1 contrast with the card background.
fn readable(fg: [f64; 3], bg: [f64; 3]) -> bool {
    let (a, b) = (relative_luminance(fg), relative_luminance(bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.0
}

fn background() -> [f64; 3] {
    srgb(&NSColor::textBackgroundColor()).unwrap_or([1.0, 1.0, 1.0])
}

fn readable_color(fg: Option<[f64; 3]>, bg: [f64; 3]) -> Retained<NSColor> {
    match fg {
        Some(c) if readable(c, bg) => NSColor::colorWithSRGBRed_green_blue_alpha(c[0], c[1], c[2], 1.0),
        _ => NSColor::labelColor(),
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn set(text: &NSMutableAttributedString, key: &NSAttributedStringKey, value: &AnyObject, range: NSRange) {
    // SAFETY: standard attribute keys with values of the documented types.
    unsafe { text.addAttribute_value_range(key, value, range) };
}

fn wrapping_paragraph() -> Retained<NSMutableParagraphStyle> {
    let p = NSMutableParagraphStyle::new();
    p.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
    p
}

fn font_for(run: &Run, target: Target, mtm: MainThreadMarker) -> Retained<NSFont> {
    let scale = [1.0, 1.45, 1.3, 1.18, 1.1, 1.0, 1.0][run.style.heading.min(6) as usize];
    let size = target.base_size() * scale;
    let font = if run.style.code {
        // SAFETY: reading AppKit constants.
        let weight = unsafe { if run.style.bold { NSFontWeightBold } else { NSFontWeightRegular } };
        NSFont::monospacedSystemFontOfSize_weight(size * 0.92, weight)
    } else if run.style.bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    if run.style.italic {
        NSFontManager::sharedFontManager(mtm).convertFont_toHaveTrait(&font, NSFontTraitMask::ItalicFontMask)
    } else {
        font
    }
}

fn from_runs(runs: &[Run], target: Target, mtm: MainThreadMarker) -> Option<Retained<NSAttributedString>> {
    if runs.is_empty() {
        return None;
    }
    let all: String = runs.iter().map(|r| r.text.as_str()).collect();
    let text = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &NSString::from_str(&all));
    let bg = background();
    let one = NSNumber::new_isize(1);
    let mut start = 0;
    for run in runs {
        let len = utf16_len(&run.text);
        let range = NSRange::new(start, len);
        start += len;
        // SAFETY: reading AppKit constants.
        unsafe {
            set(&text, NSFontAttributeName, &font_for(run, target, mtm), range);
            let color = if run.style.link { NSColor::linkColor() } else { readable_color(run.style.color, bg) };
            set(&text, NSForegroundColorAttributeName, &color, range);
            if run.style.underline || run.style.link {
                set(&text, NSUnderlineStyleAttributeName, &one, range);
            }
            if run.style.strike {
                set(&text, NSStrikethroughStyleAttributeName, &one, range);
            }
        }
    }
    if target == Target::Card {
        // SAFETY: reading an AppKit constant.
        unsafe { set(&text, NSParagraphStyleAttributeName, &wrapping_paragraph(), NSRange::new(0, text.length())) };
    }
    Some(Retained::into_super(text))
}

/// Scales fonts, fixes colors and strips what does not belong on a card.
fn normalize(source: &NSAttributedString, target: Target) -> Retained<NSAttributedString> {
    let text = NSMutableAttributedString::initWithAttributedString(NSMutableAttributedString::alloc(), source);
    let max = target.max_chars();
    if text.length() > max {
        text.deleteCharactersInRange(NSRange::new(max, text.length() - max));
    }
    let bg = background();
    let len = text.length();
    let mut fonts = Vec::new();
    let mut colors = Vec::new();
    let mut i = 0;
    while i < len {
        let mut range = NSRange::new(0, 0);
        // SAFETY: `i` is in bounds and `range` is a valid out pointer.
        let attrs = unsafe { text.attributesAtIndex_effectiveRange(i, &mut range) };
        // SAFETY: reading AppKit constants; values are checked with downcast.
        unsafe {
            let font = attrs.objectForKey(NSFontAttributeName).and_then(|f| f.downcast::<NSFont>().ok());
            let size = font.as_ref().map(|f| f.pointSize()).unwrap_or(target.base_size());
            let scaled = font
                .and_then(|f| NSFont::fontWithDescriptor_size(&f.fontDescriptor(), target.clamp_size(size)))
                .unwrap_or_else(|| NSFont::systemFontOfSize(target.clamp_size(size)));
            fonts.push((range, scaled));
            let link = attrs.objectForKey(NSLinkAttributeName).is_some();
            let fg = attrs
                .objectForKey(NSForegroundColorAttributeName)
                .and_then(|c| c.downcast::<NSColor>().ok())
                .and_then(|c| srgb(&c));
            colors.push((range, if link { NSColor::linkColor() } else { readable_color(fg, bg) }, link));
        }
        i = range.location + range.length.max(1);
    }
    let all = NSRange::new(0, len);
    // SAFETY: standard attribute keys with values of the documented types.
    unsafe {
        text.removeAttribute_range(NSBackgroundColorAttributeName, all);
        text.removeAttribute_range(NSLinkAttributeName, all);
        if target == Target::Card {
            text.removeAttribute_range(NSAttachmentAttributeName, all);
            set(&text, NSParagraphStyleAttributeName, &wrapping_paragraph(), all);
        }
        for (range, font) in fonts {
            set(&text, NSFontAttributeName, &font, range);
        }
        let one = NSNumber::new_isize(1);
        for (range, color, link) in colors {
            set(&text, NSForegroundColorAttributeName, &color, range);
            if link {
                set(&text, NSUnderlineStyleAttributeName, &one, range);
            }
        }
    }
    Retained::into_super(text)
}

/// Styled text for a rich text item, or `None` to fall back to plain text.
pub fn render(
    store: &Store,
    item: &Item,
    target: Target,
    mtm: MainThreadMarker,
) -> Option<Retained<NSAttributedString>> {
    if item.kind != ItemKind::RichText {
        return None;
    }
    if let Some(rtf) = store.representation(item.id, UTI_RTF).ok().flatten() {
        // SAFETY: RTF parsing does not touch the network or WebKit.
        let parsed = unsafe {
            NSAttributedString::initWithRTF_documentAttributes(
                NSAttributedString::alloc(),
                &NSData::with_bytes(&rtf),
                None,
            )
        };
        if let Some(parsed) = parsed.filter(|p| p.length() > 0) {
            return Some(normalize(&parsed, target));
        }
    }
    let html = store.representation(item.id, UTI_HTML).ok().flatten()?;
    let runs = html_to_runs(&String::from_utf8_lossy(&html), target.max_chars());
    from_runs(&runs, target, mtm)
}
