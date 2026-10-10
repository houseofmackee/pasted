//! The Statistics window: headline tiles and charts over a chosen range.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{NSBackingStoreType, NSSegmentSwitchTracking, NSSegmentedControl, NSWindow, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSArray, NSString};

use super::chart::{ChartView, Column, Content, Row, compact};
use super::icons::IconCache;
use super::{activate_app, ns, rect};
use crate::model::ItemKind;
use crate::store::Report;

pub const RANGES: [i64; 3] = [7, 30, 90];
const W: f64 = 920.0;
/// Full height of the content; the window is this tall when the screen
/// allows, and scrolls otherwise.
const CONTENT_H: f64 = 986.0;
const M: f64 = 24.0;
const GAP: f64 = 16.0;

pub struct StatsWindow {
    pub window: Retained<NSWindow>,
    range: Retained<NSSegmentedControl>,
    device: Retained<objc2_app_kit::NSPopUpButton>,
    /// Device keys behind the popup entries after "All devices".
    device_keys: Vec<String>,
    devices: Retained<ChartView>,
    tiles: Vec<Retained<ChartView>>,
    per_day: Retained<ChartView>,
    per_hour: Retained<ChartView>,
    kinds: Retained<ChartView>,
    apps: Retained<ChartView>,
    pasted: Retained<ChartView>,
    icons: IconCache,
}

pub fn device_label(name: &str) -> String {
    if name.is_empty() { "Unknown device".into() } else { name.to_string() }
}

fn tile(label: &str, value: String, note: String) -> Content {
    Content::Tile { label: label.into(), value, note }
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", compact(n as f64), if n == 1 { one } else { many })
}

impl StatsWindow {
    pub fn new(target: &AnyObject, mtm: MainThreadMarker) -> StatsWindow {
        let visible = objc2_app_kit::NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size.height).unwrap_or(900.0);
        let h = CONTENT_H.min(visible - 40.0).max(500.0);
        // SAFETY: standard window creation; the window is kept alive by `StatsWindow`.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, W, h),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `StatsWindow`.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns("Pasted Statistics"));
        let plane = ChartView::new(Content::Plane, rect(0.0, 0.0, W, CONTENT_H), mtm);
        let scroll =
            objc2_app_kit::NSScrollView::initWithFrame(objc2_app_kit::NSScrollView::alloc(mtm), rect(0.0, 0.0, W, h));
        scroll.setHasVerticalScroller(true);
        scroll.setDocumentView(Some(&plane));
        window.setContentView(Some(&scroll));

        let title = super::label("Statistics", 20.0, true, mtm);
        title.setFrame(rect(M, 18.0, 300.0, 28.0));
        plane.addSubview(&title);
        let labels: Vec<Retained<NSString>> = RANGES.iter().map(|d| ns(&format!("{d} days"))).collect();
        // SAFETY: the target implements `statsRange:`.
        let range = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_retained_slice(&labels),
                NSSegmentSwitchTracking::SelectOne,
                Some(target),
                Some(sel!(statsRange:)),
                mtm,
            )
        };
        range.setSelectedSegment(1);
        range.setFrame(rect(W - M - 260.0, 20.0, 260.0, 24.0));
        plane.addSubview(&range);
        let device = objc2_app_kit::NSPopUpButton::initWithFrame_pullsDown(
            objc2_app_kit::NSPopUpButton::alloc(mtm),
            rect(W - M - 260.0 - 12.0 - 200.0, 19.0, 200.0, 26.0),
            false,
        );
        device.addItemWithTitle(&ns("All devices"));
        // SAFETY: the target implements `statsRange:`.
        unsafe {
            device.setTarget(Some(target));
            device.setAction(Some(sel!(statsRange:)));
        }
        plane.addSubview(&device);

        let empty = || Content::Tile { label: String::new(), value: String::new(), note: String::new() };
        let tile_w = (W - 2.0 * M - 3.0 * 12.0) / 4.0;
        let tiles: Vec<_> = (0..4)
            .map(|i| {
                let v = ChartView::new(empty(), rect(M + i as f64 * (tile_w + 12.0), 60.0, tile_w, 88.0), mtm);
                plane.addSubview(&v);
                v
            })
            .collect();
        let half = (W - 2.0 * M - GAP) / 2.0;
        let mk = |x: f64, y: f64, w: f64, h: f64| {
            let v = ChartView::new(empty(), rect(x, y, w, h), mtm);
            plane.addSubview(&v);
            v
        };
        let per_day = mk(M, 164.0, W - 2.0 * M, 200.0);
        let per_hour = mk(M, 380.0, half, 200.0);
        let kinds = mk(M + half + GAP, 380.0, half, 200.0);
        let apps = mk(M, 596.0, half, 200.0);
        let pasted = mk(M + half + GAP, 596.0, half, 200.0);
        let devices = mk(M, 812.0, W - 2.0 * M, 150.0);
        window.center();
        StatsWindow {
            window,
            range,
            device,
            device_keys: Vec::new(),
            devices,
            tiles,
            per_day,
            per_hour,
            kinds,
            apps,
            pasted,
            icons: IconCache::default(),
        }
    }

    /// The selected device, or `None` for all devices ("" is unknown).
    pub fn device(&self) -> Option<String> {
        let i = self.device.indexOfSelectedItem();
        (i > 0).then(|| self.device_keys.get(i as usize - 1).cloned()).flatten()
    }

    /// Refills the device menu, keeping the current choice when it still exists.
    pub fn set_device_names(&mut self, names: &[(String, i64)]) {
        let keys: Vec<String> = names.iter().map(|(n, _)| n.clone()).collect();
        if keys == self.device_keys {
            return;
        }
        let current = self.device();
        self.device.removeAllItems();
        self.device.addItemWithTitle(&ns("All devices"));
        for (name, _) in names {
            self.device.addItemWithTitle(&ns(&device_label(name)));
        }
        self.device_keys = keys;
        let index = current.and_then(|c| self.device_keys.iter().position(|k| *k == c)).map_or(0, |i| i + 1);
        self.device.selectItemAtIndex(index as isize);
    }

    pub fn days(&self) -> i64 {
        RANGES.get(self.range.selectedSegment().max(0) as usize).copied().unwrap_or(30)
    }

    pub fn show(&self, mtm: MainThreadMarker) {
        activate_app(mtm);
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    pub fn update(&mut self, r: &Report) {
        let days = r.days;
        let change = if r.added_previous == 0 {
            format!("None in the {days} days before")
        } else {
            let pct = (r.added - r.added_previous) as f64 / r.added_previous as f64 * 100.0;
            let arrow = if pct >= 0.0 { "\u{2191}" } else { "\u{2193}" };
            format!("{arrow} {:.0}% vs the {days} days before", pct.abs())
        };
        let contents = [
            tile(&format!("Added in the last {days} days"), compact(r.added as f64), change),
            tile("Pasted with Pasted", compact(r.pastes as f64), "All time".into()),
            tile("In history", compact(r.history_items as f64), plural(r.pinboards, "pinboard", "pinboards")),
            tile("Storage", format!("{:.1} MB", r.bytes as f64 / 1_048_576.0), "Clips and images".into()),
        ];
        for (view, content) in self.tiles.iter().zip(contents) {
            view.set_content(content);
        }

        let n = r.per_day.len();
        let plot_w = W - 2.0 * M - 68.0;
        let every = ((n as f64 * 64.0 / plot_w).ceil() as usize).max(1);
        let columns = r
            .per_day
            .iter()
            .map(|(d, v)| Column {
                label: d.format("%b %-d").to_string(),
                value: *v as f64,
                tooltip: format!("{}: {}", d.format("%a %b %-d"), plural(*v, "item", "items")),
            })
            .collect();
        self.per_day.set_content(Content::Columns {
            title: "Items added per day".into(),
            subtitle: format!("Last {days} days. Copying something again moves it to the top instead of adding it."),
            columns,
            every_label: every,
        });

        let hour_label = |h: usize| match h {
            0 => "12 AM".to_string(),
            12 => "12 PM".to_string(),
            h if h < 12 => format!("{h} AM"),
            h => format!("{} PM", h - 12),
        };
        let columns = (0..24)
            .map(|h| Column {
                label: hour_label(h),
                value: r.per_hour[h] as f64,
                tooltip: format!(
                    "{} to {}: {}",
                    hour_label(h),
                    hour_label((h + 1) % 24),
                    plural(r.per_hour[h], "item", "items")
                ),
            })
            .collect();
        self.per_hour.set_content(Content::Columns {
            title: "Time of day".into(),
            subtitle: format!("Items added by hour, last {days} days"),
            columns,
            every_label: 6,
        });

        let total: i64 = r.by_kind.iter().map(|(_, n)| n).sum::<i64>().max(1);
        let rows = r
            .by_kind
            .iter()
            .map(|(k, n)| {
                let label = ItemKind::parse(k).map(|k| k.label().to_string()).unwrap_or_else(|| k.clone());
                let pct = *n as f64 / total as f64 * 100.0;
                let pct = if pct > 0.0 && pct < 0.5 { "<1%".to_string() } else { format!("{pct:.0}%") };
                Row {
                    tooltip: format!("{label}: {} ({pct})", plural(*n, "item", "items")),
                    value_text: compact(*n as f64),
                    label,
                    value: *n as f64,
                    icon: None,
                }
            })
            .collect();
        self.kinds.set_content(Content::Rows {
            title: "Types".into(),
            subtitle: format!("Last {days} days"),
            rows,
            empty: "Nothing copied in this period".into(),
            value_width: 52.0,
        });

        // Six apps fit; the rest fold into "Other apps".
        let shown = 6;
        let mut rows: Vec<Row> = r
            .top_apps
            .iter()
            .take(shown)
            .map(|(bundle, name, n)| Row {
                label: name.clone(),
                value: *n as f64,
                value_text: compact(*n as f64),
                icon: self.icons.app(Some(bundle)).icon.clone(),
                tooltip: format!("{name}: {}", plural(*n, "item", "items")),
            })
            .collect();
        let listed: i64 = r.top_apps.iter().take(shown).map(|(_, _, n)| n).sum();
        let other = r.added - listed;
        if r.top_apps.len() > shown && other > 0 {
            rows.push(Row {
                label: "Other apps".into(),
                value: other as f64,
                value_text: compact(other as f64),
                icon: None,
                tooltip: format!("Other apps: {}", plural(other, "item", "items")),
            });
        }
        self.apps.set_content(Content::Rows {
            title: "Copied from".into(),
            subtitle: format!("Top apps, last {days} days"),
            rows,
            empty: "Nothing copied in this period".into(),
            value_width: 52.0,
        });

        let rows = r
            .most_pasted
            .iter()
            .map(|item| Row {
                label: item.summary(60),
                value: item.use_count as f64,
                value_text: compact(item.use_count as f64),
                icon: None,
                tooltip: format!("{}: pasted {}", item.summary(80), plural(item.use_count, "time", "times")),
            })
            .collect();
        self.pasted.set_content(Content::Rows {
            title: "Most pasted".into(),
            subtitle: "All time, pasted from Pasted".into(),
            rows,
            empty: "Nothing pasted from Pasted yet".into(),
            value_width: 52.0,
        });

        let total: i64 = r.devices.iter().map(|d| d.items).sum::<i64>().max(1);
        let now = crate::model::now_ms();
        let rows = r
            .devices
            .iter()
            .map(|d| {
                let pct = d.items as f64 / total as f64 * 100.0;
                let mut detail = format!("{} ({pct:.0}%)", compact(d.items as f64));
                if let Some(app) = &d.top_app {
                    detail.push_str(&format!(", mostly {app}"));
                }
                detail.push_str(&format!(", last copy {}", crate::model::relative_time(d.last_copy.min(now))));
                Row {
                    label: device_label(&d.name),
                    value: d.items as f64,
                    tooltip: format!("{}: {detail}", device_label(&d.name)),
                    value_text: detail,
                    icon: None,
                }
            })
            .collect();
        self.devices.set_content(Content::Rows {
            title: "Devices".into(),
            subtitle: format!(
                "Items added per device, last {days} days{}",
                if r.device.is_some() { " (all devices, regardless of the filter)" } else { "" }
            ),
            rows,
            empty: "Nothing copied in this period".into(),
            value_width: 360.0,
        });
    }
}
