//! Global hotkeys through Carbon's `RegisterEventHotKey`, which works without
//! Accessibility permission. Events arrive on the main run loop.

use std::ffi::c_void;

use crate::shortcut::Shortcut;

type OSStatus = i32;
type EventTargetRef = *mut c_void;
type EventHandlerRef = *mut c_void;
type EventHandlerCallRef = *mut c_void;
type EventRef = *mut c_void;
type EventHotKeyRef = *mut c_void;
type EventHandlerUPP = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}

#[repr(C)]
#[derive(Default)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

const fn four_cc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}

const K_EVENT_CLASS_KEYBOARD: u32 = four_cc(b"keyb");
const K_EVENT_HOT_KEY_PRESSED: u32 = 5;
const K_EVENT_PARAM_DIRECT_OBJECT: u32 = four_cc(b"----");
const TYPE_EVENT_HOT_KEY_ID: u32 = four_cc(b"hkid");
const SIGNATURE: u32 = four_cc(b"PSTD");

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: EventHandlerUPP,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut EventHandlerRef,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
    fn GetEventParameter(
        event: EventRef,
        name: u32,
        desired_type: u32,
        actual_type: *mut u32,
        buffer_size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> OSStatus;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    OpenShelf = 1,
    PasteStack = 2,
}

impl HotkeyAction {
    fn from_id(id: u32) -> Option<HotkeyAction> {
        match id {
            1 => Some(HotkeyAction::OpenShelf),
            2 => Some(HotkeyAction::PasteStack),
            _ => None,
        }
    }
}

extern "C" fn handler(_next: EventHandlerCallRef, event: EventRef, _user: *mut c_void) -> OSStatus {
    let mut id = EventHotKeyID::default();
    // SAFETY: Carbon passes a valid hot key event; the buffer matches the requested type.
    let status = unsafe {
        GetEventParameter(
            event,
            K_EVENT_PARAM_DIRECT_OBJECT,
            TYPE_EVENT_HOT_KEY_ID,
            std::ptr::null_mut(),
            size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            (&mut id as *mut EventHotKeyID).cast(),
        )
    };
    if status == 0
        && id.signature == SIGNATURE
        && let Some(action) = HotkeyAction::from_id(id.id)
    {
        crate::app::on_hotkey(action);
    }
    0
}

pub struct Hotkeys {
    registered: Vec<EventHotKeyRef>,
}

impl Hotkeys {
    pub fn install() -> Hotkeys {
        let spec = EventTypeSpec { event_class: K_EVENT_CLASS_KEYBOARD, event_kind: K_EVENT_HOT_KEY_PRESSED };
        let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
        // SAFETY: valid function pointer and spec; the handler lives for the whole process.
        let status = unsafe {
            InstallEventHandler(GetApplicationEventTarget(), handler, 1, &spec, std::ptr::null_mut(), &mut handler_ref)
        };
        if status != 0 {
            tracing::error!("InstallEventHandler failed: {status}");
        }
        Hotkeys { registered: Vec::new() }
    }

    /// Replaces all registered hotkeys. Returns shortcuts that could not be registered.
    pub fn set(&mut self, bindings: &[(HotkeyAction, Shortcut)]) -> Vec<Shortcut> {
        for r in self.registered.drain(..) {
            // SAFETY: `r` came from RegisterEventHotKey and is unregistered once.
            unsafe { UnregisterEventHotKey(r) };
        }
        let mut failed = Vec::new();
        for (action, shortcut) in bindings {
            let mut r: EventHotKeyRef = std::ptr::null_mut();
            let id = EventHotKeyID { signature: SIGNATURE, id: *action as u32 };
            // SAFETY: plain values and a valid out pointer.
            let status = unsafe {
                RegisterEventHotKey(
                    shortcut.key_code as u32,
                    shortcut.modifiers.carbon(),
                    id,
                    GetApplicationEventTarget(),
                    0,
                    &mut r,
                )
            };
            if status == 0 {
                self.registered.push(r);
            } else {
                tracing::warn!("could not register {shortcut} (status {status}); is another app using it?");
                failed.push(*shortcut);
            }
        }
        failed
    }
}
