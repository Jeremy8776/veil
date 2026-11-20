//! Getting the host's attention while Veil is in the background: a notification and a
//! flashing taskbar button. Part of the Windows `ffi` module; the same rules apply (see
//! `mod.rs`).
//!
//! Purpose: someone waiting in the lobby can only be let in by the host, so the host needs to
//! know even when another app is in front.
//!
//! Invariants:
//! - Only Veil's own window is flashed or brought forward (`own_window`).
//! - Notification text goes in through XML text nodes, never pasted into XML, so a guest's
//!   chosen name cannot add markup or actions to the notification.
//! - Windows shows notifications from an app that is not packaged only under a registered app
//!   id. Veil registers [`APP_ID`] under the current user's `Software\Classes\AppUserModelId`
//!   with its display name, and nothing else.

use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastTemplateType};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
use windows::Win32::UI::WindowsAndMessaging::{
    FLASHW_STOP, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, IsIconic, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{HSTRING, PCWSTR};

use super::app_window::own_window;

/// The id Veil's notifications are shown under. Settings > Notifications lists them as Veil.
const APP_ID: &str = "Veil.VideoCalls";

/// Flashes this process's window called `title` in the taskbar until it comes to the front,
/// or stops it. False if there is no such window.
pub fn flash_taskbar(title: &str, on: bool) -> bool {
    let Some(window) = own_window(title) else {
        return false;
    };
    let info = FLASHWINFO {
        cbSize: size_of::<FLASHWINFO>() as u32,
        hwnd: window,
        dwFlags: if on {
            FLASHW_TRAY | FLASHW_TIMERNOFG
        } else {
            FLASHW_STOP
        },
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: `info` is a fully initialised FLASHWINFO with its own size, alive for the call,
    // and the window belongs to this process. The return value is the old state, not an error.
    let _ = unsafe { FlashWindowEx(&info) };
    true
}

/// Shows a notification with a bold `heading` and a `body` line. Clicking it brings this
/// process's window called `window` to the front.
pub fn show_notification(window: &str, heading: &str, body: &str) -> windows::core::Result<()> {
    register_app_id()?;
    let content = ToastNotificationManager::GetTemplateContent(ToastTemplateType::ToastText02)?;
    let lines = content.GetElementsByTagName(&HSTRING::from("text"))?;
    for (index, text) in [heading, body].into_iter().enumerate() {
        let node = content.CreateTextNode(&HSTRING::from(text))?;
        lines.Item(index as u32)?.AppendChild(&node)?;
    }
    let toast = ToastNotification::CreateToastNotification(&content)?;
    let window = window.to_owned();
    toast.Activated(&TypedEventHandler::new(move |_, _| {
        bring_forward(&window);
        Ok(())
    }))?;
    ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))?.Show(&toast)
}

/// Takes Veil's notifications out of the notification centre. Failure is harmless: they
/// expire on their own.
pub fn clear_notifications() {
    if let Ok(history) = ToastNotificationManager::History() {
        let _ = history.ClearWithId(&HSTRING::from(APP_ID));
    }
}

/// Restores this process's window called `title` if minimised and asks Windows to bring it
/// to the front. Windows may only flash it instead; either way the host sees it.
fn bring_forward(title: &str) {
    let Some(window) = own_window(title) else {
        return;
    };
    // SAFETY: the handle belongs to this process and is passed by value; each call only
    // changes that window's state, and Windows checks the handle.
    unsafe {
        if IsIconic(window).as_bool() {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let _ = SetForegroundWindow(window);
    }
}

/// Registers [`APP_ID`] with the display name "Veil" for the current user. Writing the same
/// value again is harmless, so this runs before each notification.
fn register_app_id() -> windows::core::Result<()> {
    let key = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
    let name = HSTRING::from("DisplayName");
    let value = HSTRING::from("Veil");
    // The terminating zero is part of a REG_SZ value.
    let bytes = (value.len() + 1) * size_of::<u16>();
    // SAFETY: the key, name and value are terminated wide strings that outlive the call, and
    // `bytes` covers exactly the value and its terminating zero.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            REG_SZ.0,
            Some(value.as_ptr().cast()),
            bytes as u32,
        )
    }
    .ok()
}
