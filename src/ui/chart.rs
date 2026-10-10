//! Statistics charts drawn with AppKit.
//!
//! Follows the reference data-viz palette: one series per chart in the
//! slot-1 blue (stepped separately for light and dark surfaces), thin marks
//! (columns at most 24 px with a 4 px rounded data end and a square base, a
//! 2 px gap between neighbors), solid hairline gridlines, selective labels,
//! text in text colors only, and a tooltip on every mark.

use std::cell::RefCell;
use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAppearanceCustomization, NSBezierPath, NSColor, NSFont, NSFontAttributeName,
    NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage, NSLineBreakMode,
    NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSStringDrawing, NSTextAlignment, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedStringKey, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::rect;

/// Chart colors for one appearance (from the reference palette).
struct Palette {
    surface: Retained<NSColor>,
    plane: Retained<NSColor>,
    primary: Retained<NSColor>,
    secondary: Retained<NSColor>,
    muted: Retained<NSColor>,
    grid: Retained<NSColor>,
    baseline: Retained<NSColor>,
    border: Retained<NSColor>,
    series: Retained<NSColor>,
}

fn hex(v: u32) -> Retained<NSColor> {
    let c = |s: u32| ((v >> s) & 0xff) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), 1.0)
}

impl Palette {
    fn for_view(view: &NSView) -> Palette {
        let dark = view.effectiveAppearance().name().to_string().contains("Dark");
        if dark {
            Palette {
                surface: hex(0x1a1a19),
                plane: hex(0x0d0d0d),
                primary: hex(0xf0efec),
                secondary: hex(0xc3c2b7),
                muted: hex(0x898781),
                grid: hex(0x2c2c2a),
                baseline: hex(0x383835),
                border: NSColor::colorWithWhite_alpha(1.0, 0.10),
                series: hex(0x3987e5),
            }
        } else {
            Palette {
                surface: hex(0xfcfcfb),
                plane: hex(0xf9f9f7),
                primary: hex(0x0b0b0b),
                secondary: hex(0x52514e),
                muted: hex(0x898781),
                grid: hex(0xe1e0d9),
                baseline: hex(0xc3c2b7),
                border: NSColor::colorWithWhite_alpha(0.04, 0.10),
                series: hex(0x2a78d6),
            }
        }
    }
}

#[derive(Clone)]
pub struct Column {
    /// Axis label under the column; empty to skip.
    pub label: String,
    pub value: f64,
    pub tooltip: String,
}

#[derive(Clone)]
pub struct Row {
    pub label: String,
    pub value: f64,
    pub value_text: String,
    pub icon: Option<Retained<NSImage>>,
    pub tooltip: String,
}

#[derive(Clone)]
pub enum Content {
    /// The window background.
    Plane,
    Tile {
        label: String,
        value: String,
        note: String,
    },
    Columns {
        title: String,
        subtitle: String,
        columns: Vec<Column>,
        every_label: usize,
    },
    Rows {
        title: String,
        subtitle: String,
        rows: Vec<Row>,
        empty: String,
        /// Width reserved for the value text after each bar.
        value_width: f64,
    },
}

pub struct ChartIvars {
    content: RefCell<Content>,
    /// Tooltip owners must outlive their tooltip rects.
    tooltips: RefCell<Vec<Retained<NSString>>>,
    tooltip_size: RefCell<NSSize>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "PastedChartView"]
    #[ivars = ChartIvars]
    pub struct ChartView;

    impl ChartView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }
    }

    unsafe impl NSObjectProtocol for ChartView {}
);

fn attrs(
    font: &NSFont,
    color: &NSColor,
    align: NSTextAlignment,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let p = NSMutableParagraphStyle::new();
    p.setAlignment(align);
    p.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    // SAFETY: reading AppKit constants.
    let keys: [&NSAttributedStringKey; 3] =
        unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSParagraphStyleAttributeName] };
    let values: [&AnyObject; 3] = [font, color, &p];
    NSDictionary::from_slices(&keys, &values)
}

fn text(s: &str, r: NSRect, font: &NSFont, color: &NSColor, align: NSTextAlignment) {
    // SAFETY: drawing inside drawRect: with a valid attribute dictionary.
    unsafe { NSString::from_str(s).drawInRect_withAttributes(r, Some(&attrs(font, color, align))) };
}

fn font(size: f64, semibold: bool) -> Retained<NSFont> {
    // SAFETY: reading AppKit constants.
    NSFont::systemFontOfSize_weight(size, unsafe { if semibold { NSFontWeightSemibold } else { NSFontWeightRegular } })
}

/// 1,284 / 12.9K / 4.2M
pub fn compact(v: f64) -> String {
    let a = v.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0).replace(".0M", "M")
    } else if a >= 10_000.0 {
        format!("{:.1}K", v / 1_000.0).replace(".0K", "K")
    } else if a >= 1_000.0 {
        let n = v.round() as i64;
        format!("{},{:03}", n / 1000, n % 1000)
    } else {
        format!("{}", v.round() as i64)
    }
}

/// Axis maximum and step: 1, 2 or 5 times a power of ten, about 3 ticks.
pub fn nice_scale(max: f64) -> (f64, f64) {
    if max <= 0.0 {
        return (1.0, 1.0);
    }
    let raw = max / 3.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag).max(1.0);
    ((max / step).ceil() * step, step)
}

/// A bar path with a rounded data end and a square base. In flipped
/// coordinates the column grows upward from `base_y`.
fn column_path(x: f64, w: f64, top: f64, base_y: f64) -> Retained<NSBezierPath> {
    let r = 4f64.min(w / 2.0).min((base_y - top).max(0.0));
    let p = NSBezierPath::new();
    p.moveToPoint(NSPoint::new(x, base_y));
    p.lineToPoint(NSPoint::new(x, top + r));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(x, top), NSPoint::new(x + r, top), r);
    p.lineToPoint(NSPoint::new(x + w - r, top));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(x + w, top), NSPoint::new(x + w, top + r), r);
    p.lineToPoint(NSPoint::new(x + w, base_y));
    p.closePath();
    p
}

/// A horizontal bar with its rounded end on the right.
fn row_path(x: f64, y: f64, w: f64, h: f64) -> Retained<NSBezierPath> {
    let r = 4f64.min(h / 2.0).min(w);
    let p = NSBezierPath::new();
    p.moveToPoint(NSPoint::new(x, y));
    p.lineToPoint(NSPoint::new(x + w - r, y));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(x + w, y), NSPoint::new(x + w, y + r), r);
    p.lineToPoint(NSPoint::new(x + w, y + h - r));
    p.appendBezierPathWithArcFromPoint_toPoint_radius(NSPoint::new(x + w, y + h), NSPoint::new(x + w - r, y + h), r);
    p.lineToPoint(NSPoint::new(x, y + h));
    p.closePath();
    p
}

impl ChartView {
    pub fn new(content: Content, frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ChartIvars {
            content: RefCell::new(content),
            tooltips: RefCell::new(Vec::new()),
            tooltip_size: RefCell::new(NSSize::new(0.0, 0.0)),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.update_accessibility();
        this
    }

    pub fn set_content(&self, content: Content) {
        *self.ivars().content.borrow_mut() = content;
        *self.ivars().tooltip_size.borrow_mut() = NSSize::new(0.0, 0.0);
        self.update_accessibility();
        self.setNeedsDisplay(true);
    }

    /// A text version of the data for VoiceOver.
    fn update_accessibility(&self) {
        let description = match &*self.ivars().content.borrow() {
            Content::Plane => return,
            Content::Tile { label, value, note } => format!("{label}: {value}. {note}"),
            Content::Columns { title, columns, .. } => {
                let values: Vec<String> = columns.iter().filter(|c| c.value > 0.0).map(|c| c.tooltip.clone()).collect();
                format!("{title}. {}", values.join("; "))
            }
            Content::Rows { title, rows, empty, .. } => {
                if rows.is_empty() {
                    format!("{title}. {empty}")
                } else {
                    let values: Vec<String> = rows.iter().map(|r| format!("{}: {}", r.label, r.value_text)).collect();
                    format!("{title}. {}", values.join("; "))
                }
            }
        };
        self.setAccessibilityLabel(Some(&NSString::from_str(&description)));
    }

    fn panel(&self, p: &Palette) -> NSRect {
        let b = self.bounds();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect(0.5, 0.5, b.size.width - 1.0, b.size.height - 1.0),
            10.0,
            10.0,
        );
        p.surface.setFill();
        path.fill();
        p.border.setStroke();
        path.setLineWidth(1.0);
        path.stroke();
        b
    }

    fn header(&self, p: &Palette, title: &str, subtitle: &str, w: f64) {
        text(title, rect(16.0, 12.0, w - 32.0, 18.0), &font(13.0, true), &p.primary, NSTextAlignment::Left);
        text(subtitle, rect(16.0, 30.0, w - 32.0, 16.0), &font(11.0, false), &p.muted, NSTextAlignment::Left);
    }

    /// Registers tooltip rects once per size so hover shows exact values.
    fn set_tooltips(&self, rects: Vec<(NSRect, String)>) {
        let size = self.bounds().size;
        let mut last = self.ivars().tooltip_size.borrow_mut();
        if last.width == size.width && last.height == size.height {
            return;
        }
        *last = size;
        self.removeAllToolTips();
        let mut owners = self.ivars().tooltips.borrow_mut();
        owners.clear();
        for (r, tip) in rects {
            let owner = NSString::from_str(&tip);
            // SAFETY: the owner string is kept alive in `tooltips` for as long as the rect exists.
            unsafe { self.addToolTipRect_owner_userData(r, &owner, std::ptr::null_mut::<c_void>()) };
            owners.push(owner);
        }
    }

    fn draw(&self) {
        let p = Palette::for_view(self);
        let content = self.ivars().content.borrow().clone();
        match content {
            Content::Plane => {
                p.plane.setFill();
                NSBezierPath::bezierPathWithRect(self.bounds()).fill();
            }
            Content::Tile { label, value, note } => {
                let b = self.panel(&p);
                let w = b.size.width;
                text(&label, rect(16.0, 12.0, w - 32.0, 16.0), &font(12.0, false), &p.secondary, NSTextAlignment::Left);
                text(&value, rect(16.0, 30.0, w - 32.0, 32.0), &font(26.0, true), &p.primary, NSTextAlignment::Left);
                text(&note, rect(16.0, 64.0, w - 32.0, 16.0), &font(11.0, false), &p.muted, NSTextAlignment::Left);
            }
            Content::Columns { title, subtitle, columns, every_label } => {
                let b = self.panel(&p);
                let (w, h) = (b.size.width, b.size.height);
                self.header(&p, &title, &subtitle, w);
                let (left, right, top, bottom) = (52.0, 16.0, 58.0, 26.0);
                let plot_w = w - left - right;
                let plot_h = h - top - bottom;
                let base_y = top + plot_h;
                let max = columns.iter().map(|c| c.value).fold(0.0, f64::max);
                let (axis_max, step) = nice_scale(max);
                // Gridlines and tick labels.
                let mut v = 0.0;
                while v <= axis_max + 1e-9 {
                    let y = (base_y - v / axis_max * plot_h).round() + 0.5;
                    (if v == 0.0 { &p.baseline } else { &p.grid }).setFill();
                    NSBezierPath::bezierPathWithRect(rect(left, y - 0.5, plot_w, 1.0)).fill();
                    text(
                        &compact(v),
                        rect(0.0, y - 7.0, left - 8.0, 14.0),
                        &font(10.0, false),
                        &p.muted,
                        NSTextAlignment::Right,
                    );
                    v += step;
                }
                let n = columns.len().max(1) as f64;
                let slot = plot_w / n;
                let bar_w = (slot - 2.0).clamp(1.0, 24.0);
                let peak = columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.value > 0.0)
                    .max_by(|a, b| a.1.value.total_cmp(&b.1.value))
                    .map(|(i, _)| i);
                let mut tips = Vec::new();
                p.series.setFill();
                for (i, c) in columns.iter().enumerate() {
                    let x = left + slot * i as f64 + (slot - bar_w) / 2.0;
                    if c.value > 0.0 {
                        let top_y = base_y - (c.value / axis_max * plot_h).max(1.0);
                        column_path(x, bar_w, top_y, base_y).fill();
                    }
                    tips.push((rect(left + slot * i as f64, top, slot, plot_h + bottom), c.tooltip.clone()));
                }
                // Labels: every `every_label`-th column, and the peak value.
                for (i, c) in columns.iter().enumerate() {
                    if every_label > 0 && i % every_label == 0 && !c.label.is_empty() {
                        let cx = left + slot * (i as f64 + 0.5);
                        text(
                            &c.label,
                            rect(cx - 40.0, base_y + 6.0, 80.0, 14.0),
                            &font(10.0, false),
                            &p.muted,
                            NSTextAlignment::Center,
                        );
                    }
                }
                if let Some(i) = peak {
                    let c = &columns[i];
                    let cx = left + slot * (i as f64 + 0.5);
                    let top_y = base_y - c.value / axis_max * plot_h;
                    text(
                        &compact(c.value),
                        rect(cx - 40.0, top_y - 16.0, 80.0, 14.0),
                        &font(10.0, true),
                        &p.secondary,
                        NSTextAlignment::Center,
                    );
                }
                self.set_tooltips(tips);
            }
            Content::Rows { title, subtitle, rows, empty, value_width } => {
                let b = self.panel(&p);
                let w = b.size.width;
                self.header(&p, &title, &subtitle, w);
                if rows.is_empty() {
                    text(
                        &empty,
                        rect(16.0, b.size.height / 2.0, w - 32.0, 16.0),
                        &font(12.0, false),
                        &p.muted,
                        NSTextAlignment::Center,
                    );
                    self.set_tooltips(Vec::new());
                    return;
                }
                let has_icons = rows.iter().any(|r| r.icon.is_some());
                let label_x = if has_icons { 40.0 } else { 16.0 };
                let label_w = 128.0;
                let bar_x = label_x + label_w + 8.0;
                let value_w = value_width;
                let bar_max = (w - bar_x - value_w - 16.0).max(10.0);
                let max = rows.iter().map(|r| r.value).fold(0.0, f64::max).max(1.0);
                // Rows shrink a little to fit the panel; callers fold long tails into "Other".
                let row_h = ((b.size.height - 64.0) / rows.len() as f64).clamp(18.0, 24.0);
                let mut tips = Vec::new();
                for (i, r) in rows.iter().enumerate() {
                    let y = 56.0 + i as f64 * row_h;
                    if let Some(icon) = &r.icon {
                        icon.drawInRect(rect(16.0, y + 3.0, 18.0, 18.0));
                    }
                    text(
                        &r.label,
                        rect(label_x, y + 4.0, label_w, 16.0),
                        &font(12.0, false),
                        &p.primary,
                        NSTextAlignment::Left,
                    );
                    let bw = (r.value / max * bar_max).max(2.0);
                    p.series.setFill();
                    row_path(bar_x, y + 6.0, bw, 12.0).fill();
                    text(
                        &r.value_text,
                        rect(bar_x + bw + 6.0, y + 4.0, value_w, 16.0),
                        &font(11.0, false),
                        &p.secondary,
                        NSTextAlignment::Left,
                    );
                    tips.push((rect(0.0, y, w, row_h), r.tooltip.clone()));
                }
                self.set_tooltips(tips);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_and_compact_numbers() {
        assert_eq!(nice_scale(0.0), (1.0, 1.0));
        assert_eq!(nice_scale(7.0), (10.0, 5.0));
        assert_eq!(nice_scale(42.0), (60.0, 20.0));
        assert_eq!(nice_scale(1.0), (1.0, 1.0));
        assert_eq!(nice_scale(950.0), (1000.0, 500.0));
        assert_eq!(compact(7.0), "7");
        assert_eq!(compact(1284.0), "1,284");
        assert_eq!(compact(12_900.0), "12.9K");
        assert_eq!(compact(20_000.0), "20K");
        assert_eq!(compact(4_200_000.0), "4.2M");
    }
}
