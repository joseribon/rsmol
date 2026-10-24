// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
mod viewport;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rsmol_core::session::{Effect, PRESETS, Session};
use serde::Serialize;
use tauri::{AppHandle, Manager, State, WindowEvent};
use viewport::{CameraAction, Msg};

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
async fn load_structure(
    path: String,
    viewport: State<'_, Viewport>,
    session: State<'_, Mutex<Session>>,
) -> Result<StructureInfo, String> {
    load(PathBuf::from(path), &viewport, &session).await
}

/// Downloads an entry from the RCSB PDB (cached on disk) and loads it.
#[tauri::command]
async fn fetch_structure(
    id: String,
    app: AppHandle,
    viewport: State<'_, Viewport>,
    session: State<'_, Mutex<Session>>,
) -> Result<StructureInfo, String> {
    let id = id.trim().to_ascii_uppercase();
    if !is_pdb_id(&id) {
        return Err(format!("'{id}' is not a PDB ID (four characters starting with a digit, e.g. 1HSG)"));
    }
    let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("pdb");
    let path = cache.join(format!("{id}.cif.gz"));
    if !path.exists() {
        let url = format!("https://files.rcsb.org/download/{id}.cif.gz");
        let bytes = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<u8>, String> {
            let mut response = ureq::get(&url).call().map_err(|e| match e {
                ureq::Error::StatusCode(404) => format!("{id} was not found at the RCSB PDB"),
                e => format!("download failed: {e}"),
            })?;
            // Large assemblies (ribosomes) exceed ureq's 10 MB default.
            response.body_mut().with_config().limit(512 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        // Write then rename, so an interrupted download never leaves a truncated cache entry.
        let partial = path.with_extension("partial");
        std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    }
    load(path, &viewport, &session).await
}

/// Classic four-character PDB IDs, e.g. 1HSG.
fn is_pdb_id(id: &str) -> bool {
    id.len() == 4 && id.starts_with(|c: char| c.is_ascii_digit()) && id.chars().all(|c| c.is_ascii_alphanumeric())
}

async fn load(path: PathBuf, viewport: &Viewport, session: &Mutex<Session>) -> Result<StructureInfo, String> {
    let start = Instant::now();
    let parse_path = path.clone();
    let structure = tauri::async_runtime::spawn_blocking(move || rsmol_core::io::read(&parse_path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let chains: BTreeSet<_> = structure.chains.iter().map(|c| c.id.clone()).collect();
    let info = StructureInfo {
        name: structure.name.clone(),
        path: path.to_string_lossy().into_owned(),
        atoms: structure.atom_count(),
        residues: structure.residues.len(),
        chains: chains.into_iter().collect(),
        states: structure.state_count(),
        parse_ms: start.elapsed().as_secs_f64() * 1e3,
    };
    let structure = Arc::new(structure);
    let display = {
        let mut session = session.lock().unwrap();
        session.load(structure.clone());
        session.display.clone().expect("load sets the display")
    };
    viewport.send(Msg::Load(structure, display))?;
    Ok(info)
}

#[tauri::command]
fn camera(action: CameraAction, viewport: State<'_, Viewport>) -> Result<(), String> {
    viewport.send(Msg::Camera(action))
}

#[derive(Serialize)]
struct Preset {
    name: &'static str,
    commands: &'static str,
}

/// One-click looks, with the command lines they run.
#[tauri::command]
fn presets() -> Vec<Preset> {
    PRESETS.iter().map(|&(name, commands)| Preset { name, commands }).collect()
}

#[derive(Serialize)]
struct SelectionErrorInfo {
    /// Character (not byte) offsets into the input, for the UI to underline.
    start: usize,
    end: usize,
    message: String,
}

#[derive(Serialize)]
struct SelectionSummary {
    atoms: usize,
    residues: usize,
}

#[derive(Serialize)]
struct CommandResult {
    message: String,
    canonical: Option<String>,
    atoms: Option<usize>,
    /// The "sele" selection after the command, for the selection card.
    selection: Option<SelectionSummary>,
}

/// Runs a command-bar line. With `dry_run`, only reports what it would do, for live feedback.
#[tauri::command]
fn run_command(
    text: String,
    dry_run: bool,
    viewport: State<'_, Viewport>,
    session: State<'_, Mutex<Session>>,
) -> Result<CommandResult, SelectionErrorInfo> {
    let mut session = session.lock().unwrap();
    let outcome = session.run(&text, dry_run).map_err(|e| SelectionErrorInfo {
        start: text[..e.span.start].chars().count(),
        end: text[..e.span.end].chars().count(),
        message: e.message,
    })?;
    for effect in outcome.effects {
        let msg = match effect {
            Effect::Highlight(atoms) => Msg::Highlight(atoms),
            Effect::Focus(atoms) => Msg::Focus(atoms),
            Effect::Display => Msg::Display(session.display.clone().expect("display exists with a structure")),
        };
        let _ = viewport.send(msg);
    }
    let selection = session.named.get("sele").zip(session.structure.as_ref()).map(|(set, s)| SelectionSummary {
        atoms: set.count(),
        residues: set.iter().map(|i| s.atoms.residue[i]).collect::<BTreeSet<_>>().len(),
    });
    Ok(CommandResult { message: outcome.message, canonical: outcome.canonical, atoms: outcome.atoms, selection })
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
            app.manage(Mutex::new(Session::default()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![load_structure, fetch_structure, camera, presets, run_command, startup_file])
        .run(tauri::generate_context!())
        .expect("error while running rsmol");
}

#[cfg(test)]
mod tests {
    #[test]
    fn pdb_ids() {
        assert!(super::is_pdb_id("1HSG"));
        assert!(super::is_pdb_id("4V6X"));
        assert!(!super::is_pdb_id("HSG1"));
        assert!(!super::is_pdb_id("1HS"));
        assert!(!super::is_pdb_id("1H/G"));
    }
}
