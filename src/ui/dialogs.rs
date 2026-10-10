//! Small modal dialogs. Callers must not hold the app state borrow while these
//! run, since the modal loop keeps dispatching events.

use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSFont, NSScrollView, NSTextField, NSTextView,
};
use objc2_foundation::MainThreadMarker;

use super::{ns, rect};

fn alert(title: &str, message: &str, buttons: &[&str], mtm: MainThreadMarker) -> objc2::rc::Retained<NSAlert> {
    let a = NSAlert::new(mtm);
    a.setMessageText(&ns(title));
    a.setInformativeText(&ns(message));
    for b in buttons {
        a.addButtonWithTitle(&ns(b));
    }
    NSApplication::sharedApplication(mtm).activate();
    a
}

pub fn info(title: &str, message: &str, mtm: MainThreadMarker) {
    alert(title, message, &["OK"], mtm).runModal();
}

pub fn confirm(title: &str, message: &str, action: &str, mtm: MainThreadMarker) -> bool {
    let a = alert(title, message, &[action, "Cancel"], mtm);
    a.setAlertStyle(NSAlertStyle::Warning);
    a.runModal() == NSAlertFirstButtonReturn
}

pub fn prompt(title: &str, message: &str, initial: &str, mtm: MainThreadMarker) -> Option<String> {
    let a = alert(title, message, &["OK", "Cancel"], mtm);
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(0.0, 0.0, 300.0, 24.0));
    field.setStringValue(&ns(initial));
    a.setAccessoryView(Some(&field));
    a.window().setInitialFirstResponder(Some(&field));
    (a.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
}

pub fn edit_text(title: &str, initial: &str, mtm: MainThreadMarker) -> Option<String> {
    let a = alert(title, "", &["Save", "Cancel"], mtm);
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 460.0, 260.0));
    scroll.setHasVerticalScroller(true);
    let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), rect(0.0, 0.0, 460.0, 260.0));
    text.setString(&ns(initial));
    text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    text.setRichText(false);
    text.setAllowsUndo(true);
    scroll.setDocumentView(Some(&text));
    a.setAccessoryView(Some(&scroll));
    a.window().setInitialFirstResponder(Some(&text));
    (a.runModal() == NSAlertFirstButtonReturn).then(|| text.string().to_string())
}
