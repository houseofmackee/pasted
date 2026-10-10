//! Accessibility helpers: VoiceOver descriptions and announcements, and the
//! system display preferences (reduce motion, increase contrast).

use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityAnnouncementKey, NSAccessibilityAnnouncementRequestedNotification,
    NSAccessibilityPostNotificationWithUserInfo, NSAccessibilityPriorityKey, NSAccessibilityPriorityLevel, NSView,
    NSWindowAnimationBehavior, NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use crate::model::{Item, ItemKind, now_ms, relative_time};

pub fn voiceover_on() -> bool {
    NSWorkspace::sharedWorkspace().isVoiceOverEnabled()
}

pub fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

pub fn increase_contrast() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast()
}

/// Window animation that respects Reduce Motion.
pub fn animation_behavior() -> NSWindowAnimationBehavior {
    if reduce_motion() { NSWindowAnimationBehavior::None } else { NSWindowAnimationBehavior::Default }
}

/// Speaks `text` with VoiceOver (no effect when VoiceOver is off).
pub fn announce(element: &AnyObject, text: &str) {
    if !voiceover_on() || text.is_empty() {
        return;
    }
    let message = NSString::from_str(text);
    let priority = NSNumber::new_isize(NSAccessibilityPriorityLevel::High.0);
    // SAFETY: reading AppKit constants; the dictionary holds the documented value types.
    unsafe {
        let info = NSDictionary::from_slices(
            &[NSAccessibilityAnnouncementKey, NSAccessibilityPriorityKey],
            &[&*message as &AnyObject, &*priority as &AnyObject],
        );
        NSAccessibilityPostNotificationWithUserInfo(
            element,
            NSAccessibilityAnnouncementRequestedNotification,
            Some(&info),
        );
    }
}

/// Makes `view` one accessibility element by hiding its subviews from
/// assistive technology (the view's own label describes them).
pub fn hide_children(view: &NSView) {
    for child in view.subviews().iter() {
        child.setAccessibilityElement(false);
        hide_children(&child);
    }
}

/// What VoiceOver says for a card. Sensitive items never reveal their text.
pub fn describe_item(item: &Item, position: usize, total: usize, close_match: bool) -> String {
    let mut parts = Vec::new();
    if item.sensitive {
        parts.push("Sensitive item, hidden".to_string());
    } else {
        let kind = match item.kind {
            ItemKind::RichText => "Text".to_string(),
            k => k.label().to_string(),
        };
        let what = match &item.title {
            Some(t) if !t.is_empty() => format!("{kind}, {t}"),
            _ => kind,
        };
        parts.push(what);
    }
    let mut origin = String::new();
    if let Some(app) = &item.app_name {
        origin.push_str(&format!("from {app}, "));
    }
    origin.push_str(&relative_time(item.timestamp));
    parts.push(origin);
    if !item.sensitive {
        parts.push(item.summary(160));
    }
    if let Some(at) = item.expires_at {
        parts.push(format!("expires in {}", crate::rules::format_duration(at - now_ms())));
    }
    if close_match {
        parts.push("close match".into());
    }
    parts.push(format!("item {} of {}", position + 1, total));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ItemMeta;

    fn item(kind: ItemKind, text: &str) -> Item {
        Item {
            id: 1,
            kind,
            title: None,
            text: text.into(),
            meta: ItemMeta::default(),
            app_bundle_id: Some("com.google.Chrome".into()),
            app_name: Some("Google Chrome".into()),
            device: None,
            created_at: now_ms(),
            timestamp: now_ms(),
            use_count: 0,
            size_bytes: 0,
            expires_at: None,
            sensitive: false,
        }
    }

    #[test]
    fn describes_items_for_voiceover() {
        let d = describe_item(&item(ItemKind::Link, "https://github.com/x"), 2, 150, false);
        assert_eq!(d, "Link, from Google Chrome, just now, https://github.com/x, item 3 of 150");
        let mut titled = item(ItemKind::RichText, "body");
        titled.title = Some("Release notes".into());
        assert!(describe_item(&titled, 0, 1, true).starts_with("Text, Release notes, from Google Chrome"));
        assert!(describe_item(&titled, 0, 1, true).ends_with("close match, item 1 of 1"));
    }

    #[test]
    fn sensitive_items_are_not_read_out() {
        let mut secret = item(ItemKind::Text, "hunter2-password");
        secret.sensitive = true;
        secret.expires_at = Some(now_ms() + 120_000);
        let d = describe_item(&secret, 0, 3, false);
        assert!(d.starts_with("Sensitive item, hidden, from Google Chrome"));
        assert!(d.contains("expires in"));
        assert!(!d.contains("hunter2"));
    }
}
