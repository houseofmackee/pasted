//! Launch at login through `SMAppService` (macOS 13+). Only works when running
//! from the `Pasted.app` bundle, not from `cargo run`.

use objc2_foundation::NSBundle;
use objc2_service_management::{SMAppService, SMAppServiceStatus};

fn in_app_bundle() -> bool {
    NSBundle::mainBundle().bundlePath().to_string().ends_with(".app")
}

/// Makes the login item match `enabled`. Returns a message when it could not.
pub fn sync(enabled: bool) -> Option<String> {
    if !in_app_bundle() {
        return enabled.then(|| "launch_at_login only works when running Pasted.app".to_string());
    }
    // SAFETY: plain ServiceManagement calls for the main app bundle.
    unsafe {
        let service = SMAppService::mainAppService();
        let registered = matches!(service.status(), SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval);
        let result = match (enabled, registered) {
            (true, false) => service.registerAndReturnError(),
            (false, true) => service.unregisterAndReturnError(),
            _ => Ok(()),
        };
        result.err().map(|e| format!("launch at login: {}", e.localizedDescription()))
    }
}
