//! rsmol desktop app: a WGPUI interface around the native wgpu viewport, the same on macOS,
//! Linux and Windows.
//!
//! `rsmol-desktop [file]` opens a structure, `.rsmol` session or `.py` script at start.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod backend;
#[cfg(target_os = "macos")]
mod macos;
mod scripting;
mod text_input;
mod theme;
mod viewport;

use std::path::PathBuf;
use std::sync::Arc;

use wgpui::{App, AppContext, Application, Bounds, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowKind, WindowOptions, point, px, size};

use crate::app::RsmolApp;
use crate::backend::Backend;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let startup = std::env::args().skip(1).find(|a| !a.starts_with('-')).map(PathBuf::from);

    Application::new().with_assets(theme::Assets).run(move |cx: &mut App| {
        if let Err(e) = cx.text_system().add_fonts(theme::Assets::fonts()) {
            eprintln!("rsmol: could not load the bundled fonts: {e}");
        }
        app::bind_keys(cx);
        text_input::bind_keys(cx);
        cx.set_menus(vec![Menu {
            name: "rsmol".into(),
            disabled: false,
            items: vec![MenuItem::action("Open…", app::OpenFile), MenuItem::action("Save Session…", app::SaveFile), MenuItem::action("Quit rsmol", app::Quit)],
        }]);
        cx.on_window_closed(|cx| cx.quit()).detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.0), px(800.0)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("rsmol".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.0), px(13.0))) }),
            window_min_size: Some(size(px(640.0), px(420.0))),
            // Matches the .desktop file name, so Linux desktops show the right icon and name.
            app_id: Some("io.github.joseribon.rsmol".into()),
            // Development aid: a window that floats above others can be screenshotted while
            // a terminal has focus (a covered window is never painted).
            kind: if std::env::var_os("RSMOL_ON_TOP").is_some() { WindowKind::PopUp } else { WindowKind::Normal },
            ..Default::default()
        };
        let window = cx
            .open_window(options, |window, cx| {
                #[cfg(target_os = "macos")]
                macos::transparent_titlebar(window);
                let bounds = window.viewport_size();
                let scale = window.scale_factor();
                let surface = window
                    .create_wgpu_surface((f32::from(bounds.width) * scale) as u32, (f32::from(bounds.height) * scale) as u32, viewport::FORMAT)
                    .expect("this platform cannot host a wgpu surface");
                let gpu = window.gpu_specs().map_or_else(|| "GPU".to_owned(), |g| g.device_name);
                let (events, view_events) = futures::channel::mpsc::unbounded();
                let viewport = viewport::spawn(surface.clone(), gpu, events);
                let backend = Arc::new(Backend::new(viewport));
                cx.new(|cx| RsmolApp::new(backend, surface, view_events, window, cx))
            })
            .expect("could not open the window");
        cx.activate(true);

        if let Some(path) = startup {
            if !app::is_openable(&path) {
                eprintln!("rsmol: {} is not a structure, session or script file", path.display());
            }
            let path = if path.is_relative() { std::env::current_dir().map(|d| d.join(&path)).unwrap_or(path) } else { path };
            if let Ok(view) = window.update(cx, |_, _, cx| cx.entity()) {
                app::open_at_start(&view, path, cx);
            }
        }
    });
}
