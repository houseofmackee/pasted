//! App icons and their dominant colors, used for card headers.

use std::collections::HashMap;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{NSBitmapImageRep, NSColor, NSColorSpace, NSImage, NSWorkspace};
use objc2_foundation::NSSize;

use super::{ns, rect};

pub struct AppLook {
    pub icon: Option<Retained<NSImage>>,
    pub color: Retained<NSColor>,
}

#[derive(Default)]
pub struct IconCache {
    apps: HashMap<String, AppLook>,
}

fn fallback_color() -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.42, 0.45, 0.5, 1.0)
}

/// Most common saturated hue of a small rendition of the icon, so multi-color
/// icons get one of their colors instead of a muddy average.
fn dominant_color(icon: &NSImage) -> Option<Retained<NSColor>> {
    let mut r = rect(0.0, 0.0, 32.0, 32.0);
    // SAFETY: `r` is a valid rect for the duration of the call.
    let cg = unsafe { icon.CGImageForProposedRect_context_hints(&mut r, None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg);
    let (w, h) = (rep.pixelsWide(), rep.pixelsHigh());
    let srgb = NSColorSpace::sRGBColorSpace();
    let step = ((w.max(h) / 24).max(1)) as usize;
    let mut pixels = Vec::new();
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let Some(c) = rep.colorAtX_y(x, y).and_then(|c| c.colorUsingColorSpace(&srgb)) else { continue };
            let (mut cr, mut cg_, mut cb, mut ca) = (0.0, 0.0, 0.0, 0.0);
            // SAFETY: valid out pointers.
            unsafe { c.getRed_green_blue_alpha(&mut cr, &mut cg_, &mut cb, &mut ca) };
            if ca >= 0.5 {
                pixels.push([cr, cg_, cb]);
            }
        }
    }
    let [r, g, b] = pick_dominant(&pixels)?;
    Some(NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0))
}

/// Buckets saturated pixels by hue and averages the largest bucket. Falls
/// back to the overall average for grayscale icons. Darkens very light
/// results so white header text stays readable.
pub fn pick_dominant(pixels: &[[f64; 3]]) -> Option<[f64; 3]> {
    if pixels.is_empty() {
        return None;
    }
    const BUCKETS: usize = 12;
    let mut sums = [[0.0f64; 4]; BUCKETS];
    for &[r, g, b] in pixels {
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let chroma = max - min;
        if chroma < 0.2 || max < 0.15 {
            continue;
        }
        let hue = if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        } / 6.0;
        let bucket = ((hue * BUCKETS as f64) as usize).min(BUCKETS - 1);
        sums[bucket][0] += r;
        sums[bucket][1] += g;
        sums[bucket][2] += b;
        sums[bucket][3] += 1.0;
    }
    let best = sums.iter().max_by(|a, b| a[3].total_cmp(&b[3]))?;
    let mut c = if best[3] >= pixels.len() as f64 * 0.04 {
        [best[0] / best[3], best[1] / best[3], best[2] / best[3]]
    } else {
        let n = pixels.len() as f64;
        let s = pixels.iter().fold([0.0; 3], |acc, p| [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]]);
        [s[0] / n, s[1] / n, s[2] / n]
    };
    let luma = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    if luma > 0.55 {
        let k = 0.55 / luma;
        c = c.map(|v| v * k);
    }
    Some(c)
}

impl IconCache {
    pub fn app(&mut self, bundle_id: Option<&str>) -> &AppLook {
        let key = bundle_id.unwrap_or("").to_string();
        self.apps.entry(key).or_insert_with(|| {
            let icon = bundle_id.and_then(|id| {
                let ws = NSWorkspace::sharedWorkspace();
                let url = ws.URLForApplicationWithBundleIdentifier(&ns(id))?;
                let path = url.path()?;
                Some(ws.iconForFile(&path))
            });
            let color = icon.as_deref().and_then(dominant_color).unwrap_or_else(fallback_color);
            if let Some(icon) = &icon {
                icon.setSize(NSSize::new(64.0, 64.0));
            }
            AppLook { icon, color }
        })
    }

    pub fn file_icon(path: &str) -> Retained<NSImage> {
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&ns(path));
        icon.setSize(NSSize::new(128.0, 128.0));
        icon
    }
}

#[cfg(test)]
mod tests {
    use super::pick_dominant;

    #[test]
    fn dominant_hue_wins_over_average() {
        // Mostly red with some green and blue: expect red, not a brown average.
        let mut px = vec![[0.9, 0.1, 0.1]; 50];
        px.extend(vec![[0.1, 0.8, 0.2]; 30]);
        px.extend(vec![[0.1, 0.2, 0.9]; 20]);
        let [r, g, b] = pick_dominant(&px).unwrap();
        assert!(r > g * 3.0 && r > b * 3.0, "{r} {g} {b}");
    }

    #[test]
    fn grayscale_falls_back_to_average_and_light_is_darkened() {
        let [r, g, b] = pick_dominant(&[[1.0, 1.0, 1.0], [0.9, 0.9, 0.9]]).unwrap();
        assert!((r - g).abs() < 1e-9 && (g - b).abs() < 1e-9);
        assert!(0.299 * r + 0.587 * g + 0.114 * b <= 0.55 + 1e-9);
        assert!(pick_dominant(&[]).is_none());
    }
}
