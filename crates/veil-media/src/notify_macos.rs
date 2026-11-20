//! Notifications on macOS, through the User Notifications framework.
//!
//! Purpose: the macOS side of [`crate::attention`].
//!
//! Invariants:
//! - Only inside an app bundle (Veil.app). Outside one, as with `cargo run`, the framework
//!   raises an exception that ends the process, so nothing is called there.
//! - macOS asks the person once whether Veil may show notifications and remembers the
//!   answer. A notification is added only once permission is given.
//! - Each new notification replaces the last one ([`ID`]), so at most one is listed.
//! - Clicking a notification opens Veil, which macOS does for the bundle by itself.
//! - These framework calls are safe Rust in `objc2`, so this module needs no `unsafe`.

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

/// The identifier every lobby notification is shown under.
const ID: &str = "veil.lobby";

fn in_bundle() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

/// Shows a notification. `_window` is unused: clicking it opens Veil.
pub fn show_notification(_window: &str, heading: &str, body: &str) -> Result<(), String> {
    if !in_bundle() {
        return Err("not running from Veil.app".into());
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(heading));
    content.setBody(&NSString::from_str(body));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(ID),
        &content,
        None,
    );
    let add = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        if granted.as_bool() {
            UNUserNotificationCenter::currentNotificationCenter()
                .addNotificationRequest_withCompletionHandler(&request, None);
        }
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &add,
        );
    Ok(())
}

/// Takes Veil's notifications out of Notification Centre.
pub fn clear_notifications() {
    if in_bundle() {
        UNUserNotificationCenter::currentNotificationCenter().removeAllDeliveredNotifications();
    }
}
