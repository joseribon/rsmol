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

use rsmol_core::session::{Effect, PRESETS, Session, Source};
use rsmol_core::session_file::CameraState;
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

/// Loads a structure file, or restores a `.rsmol` session (display, selections, measurements
/// and camera included).
async fn load(path: PathBuf, viewport: &Viewport, session: &Mutex<Session>) -> Result<StructureInfo, String> {
    let start = Instant::now();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
    let is_session = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(rsmol_core::session_file::EXTENSION));
    let read_path = path.clone();
    let (loaded, camera) = tauri::async_runtime::spawn_blocking(move || -> Result<(Session, Option<CameraState>), String> {
        let bytes = std::fs::read(&read_path).map_err(|e| format!("{}: {e}", read_path.display()))?;
        if is_session {
            return Session::open(&bytes);
        }
        let structure = rsmol_core::io::parse_bytes(&bytes, &file_name).map_err(|e| e.to_string())?;
        let mut s = Session::default();
        s.load_with_source(Arc::new(structure), Source { file_name, data: Arc::new(bytes) });
        Ok((s, None))
    })
    .await
    .map_err(|e| e.to_string())??;

    // A structure file joins the current session as a new object; a session file replaces it.
    let structure = if is_session {
        loaded.structure.clone().expect("sessions hold structures")
    } else {
        loaded.objects[0].structure.clone()
    };
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
    let mut session = session.lock().unwrap();
    if is_session {
        *session = loaded;
        let highlight = session.named.get("sele").map(|set| set.iter().collect()).unwrap_or_default();
        let effects = vec![Effect::Scene { refit: true }, Effect::Measurements, Effect::Highlight(highlight)];
        apply_effects(&session, effects, viewport);
        if let Some(camera) = camera {
            viewport.send(Msg::SetCamera(camera))?;
        }
    } else {
        let object = loaded.objects.into_iter().next().expect("loading made one object");
        let (_, effects) = session.add_object(object.structure, object.source);
        apply_effects(&session, effects, viewport);
    }
    Ok(info)
}

/// Forwards a command's effects to the render thread.
fn apply_effects(session: &Session, effects: Vec<Effect>, viewport: &Viewport) {
    for effect in effects {
        let msg = match effect {
            Effect::Highlight(atoms) => Msg::Highlight(atoms),
            Effect::Focus(atoms) => Msg::Focus(atoms),
            Effect::Display => match session.render_display() {
                Some(display) => Msg::Display(display),
                None => continue,
            },
            Effect::Measurements => Msg::Measurements(session.measurements.clone()),
            Effect::Scene { refit } => {
                Msg::Scene { structure: session.structure.clone(), display: session.render_display(), refit }
            }
        };
        let _ = viewport.send(msg);
    }
}

/// Saves the session (structure, display, selections, measurements, camera) to `path`.
#[tauri::command]
async fn save_session(path: String, viewport: State<'_, Viewport>, session: State<'_, Mutex<Session>>) -> Result<String, String> {
    let (reply, answer) = std::sync::mpsc::channel();
    viewport.send(Msg::GetCamera(reply))?;
    let camera = tauri::async_runtime::spawn_blocking(move || answer.recv_timeout(std::time::Duration::from_secs(1)).ok())
        .await
        .map_err(|e| e.to_string())?;
    let bytes = session.lock().unwrap().save(camera)?;
    let mut path = PathBuf::from(path);
    if path.extension().is_none() {
        path.set_extension(rsmol_core::session_file::EXTENSION);
    }
    // Write then rename, so a failed save never truncates an existing session.
    let partial = path.with_extension("rsmol.partial");
    std::fs::write(&partial, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    Ok(format!("saved {} ({:.1} MB)", path.display(), bytes.len() as f64 / 1e6))
}

/// The session state the UI mirrors (selection, measurements, undo), e.g. after opening a session.
#[tauri::command]
fn session_state(session: State<'_, Mutex<Session>>) -> CommandResult {
    state_result(&session.lock().unwrap(), String::new(), None, None)
}

#[tauri::command]
fn camera(action: CameraAction, viewport: State<'_, Viewport>) -> Result<(), String> {
    viewport.send(Msg::Camera(action))
}

#[derive(Serialize)]
struct PickInfo {
    /// 1-based, as the `index` selection keyword expects.
    index: usize,
    label: String,
    chain: String,
    residue: String,
    atom: String,
    element: String,
    b_factor: f32,
}

/// The atom under physical pixel (`x`, `y`), if any.
#[tauri::command]
async fn pick(
    x: f32,
    y: f32,
    viewport: State<'_, Viewport>,
    session: State<'_, Mutex<Session>>,
) -> Result<Option<PickInfo>, String> {
    if x < 0.0 || y < 0.0 {
        return Ok(None);
    }
    let (reply, answer) = std::sync::mpsc::channel();
    viewport.send(Msg::Pick { x: x as u32, y: y as u32, reply })?;
    let atom = tauri::async_runtime::spawn_blocking(move || answer.recv_timeout(std::time::Duration::from_millis(250)).ok().flatten())
        .await
        .map_err(|e| e.to_string())?;
    let session = session.lock().unwrap();
    let (Some(i), Some(s)) = (atom, session.structure.as_ref()) else { return Ok(None) };
    if i >= s.atom_count() {
        return Ok(None);
    }
    let r = s.residue_of(i);
    Ok(Some(PickInfo {
        index: i + 1,
        label: s.atom_label(i),
        chain: s.chain_of(i).id.clone(),
        residue: format!("{} {}{}", r.name, r.seq_num, r.ins_code.map(String::from).unwrap_or_default()),
        atom: s.atoms.name[i].clone(),
        element: s.atoms.element[i].symbol().to_owned(),
        b_factor: s.atoms.b_factor[i],
    }))
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
struct MeasurementSummary {
    name: String,
    label: String,
}

#[derive(Serialize)]
struct ObjectSummary {
    name: String,
    atoms: usize,
    residues: usize,
    chains: Vec<String>,
    enabled: bool,
}

#[derive(Serialize)]
struct CommandResult {
    message: String,
    canonical: Option<String>,
    atoms: Option<usize>,
    /// The "sele" selection after the command, for the selection card.
    selection: Option<SelectionSummary>,
    measurements: Vec<MeasurementSummary>,
    can_undo: bool,
    can_redo: bool,
    objects: Vec<ObjectSummary>,
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
    apply_effects(&session, outcome.effects, &viewport);
    Ok(state_result(&session, outcome.message, outcome.canonical, outcome.atoms))
}

fn state_result(session: &Session, message: String, canonical: Option<String>, atoms: Option<usize>) -> CommandResult {
    let selection = session.named.get("sele").zip(session.structure.as_ref()).map(|(set, s)| SelectionSummary {
        atoms: set.count(),
        residues: set.iter().map(|i| s.atoms.residue[i]).collect::<BTreeSet<_>>().len(),
    });
    let measurements =
        session.measurements.iter().map(|m| MeasurementSummary { name: m.name.clone(), label: m.label() }).collect();
    CommandResult {
        message,
        canonical,
        atoms,
        selection,
        measurements,
        can_undo: session.can_undo(),
        can_redo: session.can_redo(),
        objects: session
            .objects
            .iter()
            .map(|o| ObjectSummary {
                name: o.name.clone(),
                atoms: o.structure.atom_count(),
                residues: o.structure.residues.len(),
                chains: o.structure.chains.iter().map(|c| c.id.clone()).collect::<BTreeSet<_>>().into_iter().collect(),
                enabled: o.enabled,
            })
            .collect(),
    }
}

/// A structure path passed on the command line, if any.
#[tauri::command]
fn startup_file() -> Option<String> {
    std::env::args().skip(1).find(|a| !a.starts_with('-'))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
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
        .invoke_handler(tauri::generate_handler![load_structure, fetch_structure, save_session, session_state, camera, pick, presets, run_command,
            startup_file])
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
