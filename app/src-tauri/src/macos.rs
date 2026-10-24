//! AppKit view hosting the Metal layer.
//!
//! wry replaces the window's original content view with its own container holding the
//! WKWebView, so the window handle Tauri exposes points at a view that is no longer on screen.
//! Instead we add a dedicated view to wry's container, ordered below the webview, and render
//! into that: the transparent web UI then composites on top of the molecule.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindow, NSWindowOrderingMode};
use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};
use tauri::WebviewWindow;

/// Inserts a full-size view beneath the webview and returns a surface target for it.
/// Must be called on the main thread.
pub fn metal_host_view(window: &WebviewWindow) -> Result<wgpu::SurfaceTargetUnsafe, String> {
    let mtm = MainThreadMarker::new().ok_or("the viewport must be created on the main thread")?;
    let ns_window = window.ns_window().map_err(|e| e.to_string())?.cast::<NSWindow>();
    // SAFETY: Tauri returns the live NSWindow of this webview window, and we are on the main
    // thread, which AppKit requires.
    let ns_window = unsafe { ns_window.as_ref() }.ok_or("window has no NSWindow")?;
    let container = ns_window.contentView().ok_or("window has no content view")?;

    let view = NSView::initWithFrame(NSView::alloc(mtm), container.bounds());
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    view.setWantsLayer(true);
    // `relativeTo: None` orders it below every existing subview, i.e. under the WKWebView.
    container.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);

    // The container now retains the view for the window's lifetime, so the raw pointer stays
    // valid after our `Retained` is dropped.
    let ptr = NonNull::new(objc2::rc::Retained::as_ptr(&view) as *mut c_void).ok_or("null NSView")?;
    Ok(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr)),
    })
}
