// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
mod viewport;

use std::collections::BTreeSet;
use std::sync::mpsc::Sender;
use std::time::Instant;

use serde::Serialize;
use tauri::{Manager, State, WindowEvent};
use viewport::{CameraAction, Msg, RepresentationArg};

struct Viewport(Sender<Msg>);

impl Viewport {
    fn send(&self, msg: Msg) -> Result<(), String> {
        self.0.send(msg).map_err(|_| "viewport thread has stopped".to_owned())
    }
}

#[derive(Serialize)]
struct StructureInfo {
    name: String,
    path: String,
    atoms: usize,
    residues: usize,
    chains: Vec<String>,
    states: usize,
    parse_ms: f64,
}

#[tauri::command]
async fn load_structure(path: String, viewport: State<'_, Viewport>) -> Result<StructureInfo, String> {
    let start = Instant::now();
    let parse_path = path.clone();
    let structure = tauri::async_runtime::spawn_blocking(move || rsmol_core::io::read(&parse_path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let chains: BTreeSet<_> = structure.chains.iter().map(|c| c.id.clone()).collect();
    let info = StructureInfo {
        name: structure.name.clone(),
        path,
        atoms: structure.atom_count(),
        residues: structure.residues.len(),
        chains: chains.into_iter().collect(),
        states: structure.state_count(),
        parse_ms: start.elapsed().as_secs_f64() * 1e3,
    };
    viewport.send(Msg::Load(Box::new(structure)))?;
    Ok(info)
}

#[tauri::command]
fn camera(action: CameraAction, viewport: State<'_, Viewport>) -> Result<(), String> {
    viewport.send(Msg::Camera(action))
}

#[tauri::command]
fn set_representation(representation: RepresentationArg, viewport: State<'_, Viewport>) -> Result<(), String> {
    viewport.send(Msg::Representation(representation.into()))
}

#[derive(Serialize)]
struct SelectionErrorInfo {
    /// Character (not byte) offsets into the input, for the UI to underline.
    start: usize,
    end: usize,
    message: String,
}

/// Parses a selection and returns its canonical form. Evaluation is not wired up yet.
#[tauri::command]
fn parse_selection(text: String) -> Result<String, SelectionErrorInfo> {
    rsmol_core::select::parse(&text).map(|e| e.to_string()).map_err(|e| SelectionErrorInfo {
        start: text[..e.span.start].chars().count(),
        end: text[..e.span.end].chars().count(),
        message: e.message,
    })
}

/// A structure path passed on the command line, if any.
#[tauri::command]
fn startup_file() -> Option<String> {
    std::env::args().skip(1).find(|a| !a.starts_with('-'))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let window = app.get_webview_window("main").ok_or("main window missing")?;
            let tx = viewport::spawn(&window, app.handle().clone())?;
            let resize_tx = tx.clone();
            window.on_window_event(move |event| {
                if let WindowEvent::Resized(size) = event {
                    let _ = resize_tx.send(Msg::Resize { width: size.width, height: size.height });
                }
            });
            app.manage(Viewport(tx));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![load_structure, camera, set_representation, parse_selection, startup_file])
        .run(tauri::generate_context!())
        .expect("error while running rsmol");
}
