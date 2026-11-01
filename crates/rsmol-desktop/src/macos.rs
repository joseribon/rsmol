//! macOS window chrome: the content runs under a transparent title bar, so the traffic lights
//! float over the app's own header as in the earlier app. WGPUI does not apply its
//! `appears_transparent` option yet, so the NSWindow is configured directly.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSView, NSWindowStyleMask, NSWindowTitleVisibility};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// Must be called on the main thread, with the window open.
pub fn transparent_titlebar(window: &impl HasWindowHandle) {
    let Some(_mtm) = MainThreadMarker::new() else { return };
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return };
    // SAFETY: the handle points at the window's live content view, and we are on the main
    // thread, which AppKit requires.
    let view = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else { return };
    window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
}
