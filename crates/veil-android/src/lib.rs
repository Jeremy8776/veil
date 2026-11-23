//! Android entry point. All call and security code is shared with the desktop application.

#[cfg(target_os = "android")]
// SAFETY: android-activity requires exactly this exported entry point and owns AndroidApp.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    if let Err(problem) = veil_app::run_android(app) {
        eprintln!("Veil Android stopped: {problem}");
    }
}
