//! Simulating Cmd+V in the frontmost app, which needs Accessibility permission.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;

const KEY_V: u16 = 9;
const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
const KCG_HID_EVENT_TAP: u32 = 0;
const KCG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const AnyObject) -> bool;
    fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, key: u16, down: bool) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

pub fn is_trusted() -> bool {
    // SAFETY: no arguments.
    unsafe { AXIsProcessTrusted() }
}

/// Shows the system prompt that leads to System Settings > Accessibility.
pub fn request_trust() -> bool {
    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let value = NSNumber::new_bool(true);
    let options: Retained<NSDictionary<NSString, NSNumber>> = NSDictionary::from_slices(&[&*key], &[&*value]);
    // SAFETY: NSDictionary is toll-free bridged with CFDictionary.
    unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options).cast()) }
}

/// Presses the left arrow `n` times (used to place the cursor after a snippet).
pub fn post_left_arrows(n: usize) {
    const KEY_LEFT: u16 = 123;
    if !is_trusted() {
        return;
    }
    // SAFETY: standard CoreGraphics event creation; every created object is released.
    unsafe {
        let source = CGEventSourceCreate(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION);
        for _ in 0..n.min(5000) {
            for down in [true, false] {
                let event = CGEventCreateKeyboardEvent(source, KEY_LEFT, down);
                if event.is_null() {
                    continue;
                }
                // Clear modifiers so a held key does not turn this into a selection.
                CGEventSetFlags(event, 0);
                CGEventPost(KCG_HID_EVENT_TAP, event);
                CFRelease(event);
            }
        }
        if !source.is_null() {
            CFRelease(source);
        }
    }
}

/// Posts Cmd+V. Returns false if Accessibility permission is missing.
pub fn post_cmd_v() -> bool {
    if !is_trusted() {
        return false;
    }
    // SAFETY: standard CoreGraphics event creation; every created object is released.
    unsafe {
        let source = CGEventSourceCreate(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION);
        for down in [true, false] {
            let event = CGEventCreateKeyboardEvent(source, KEY_V, down);
            if event.is_null() {
                continue;
            }
            CGEventSetFlags(event, KCG_EVENT_FLAG_MASK_COMMAND);
            CGEventPost(KCG_HID_EVENT_TAP, event);
            CFRelease(event);
        }
        if !source.is_null() {
            CFRelease(source);
        }
    }
    true
}
