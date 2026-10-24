// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod scripting;
mod viewport;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rsmol_core::session::{Effect, PRESETS, Session, Source};
use rsmol_core::session_file::CameraState;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use viewport::{CameraAction, Msg};

pub(crate) struct Viewport(Sender<Msg>);

impl Viewport {
    pub(crate) fn send(&self, msg: Msg) -> Result<(), String> {
        self.0.send(msg).map_err(|_| "viewport thread has stopped".to_owned())
    }
}

#[derive(Serialize)]
pub(crate) struct StructureInfo {
    name: String,
    /// The object name it was loaded as (made unique, or `name` for sessions).
    pub(crate) object: String,
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
    load(PathBuf::from(path), None, &viewport, &session).await
}

/// Downloads an entry from the RCSB PDB (cached on disk) and loads it.
#[tauri::command]
async fn fetch_structure(
    id: String,
    app: AppHandle,
    viewport: State<'_, Viewport>,
    session: State<'_, Mutex<Session>>,
) -> Result<StructureInfo, String> {
    let path = fetch_to_cache(&app, &id).await?;
    load(path, None, &viewport, &session).await
}

/// Downloads `<ID>.cif.gz` from the RCSB PDB into the app cache, unless already there.
pub(crate) async fn fetch_to_cache(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
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
    Ok(path)
}

/// Classic four-character PDB IDs, e.g. 1HSG.
fn is_pdb_id(id: &str) -> bool {
    id.len() == 4 && id.starts_with(|c: char| c.is_ascii_digit()) && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Loads a structure file, or restores a `.rsmol` session (display, selections, measurements
/// and camera included).
/// `object` names the new object (structure files only).
pub(crate) async fn load(
    path: PathBuf,
    object: Option<String>,
    viewport: &Viewport,
    session: &Mutex<Session>,
) -> Result<StructureInfo, String> {
    let start = Instant::now();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
    let is_session = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(rsmol_core::session_file::EXTENSION));
    let read_path = path.clone();
    let (loaded, camera) = tauri::async_runtime::spawn_blocking(move || -> Result<(Session, Option<CameraState>), String> {
        let bytes = std::fs::read(&read_path).map_err(|e| format!("{}: {e}", read_path.display()))?;
        if is_session {
            return Session::open(&bytes);
        }
        let mut structure = rsmol_core::io::parse_bytes(&bytes, &file_name).map_err(|e| e.to_string())?;
        if let Some(name) = object {
            structure.name = name;
        }
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
    let mut info = StructureInfo {
        name: structure.name.clone(),
        object: structure.name.clone(),
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
        let (name, effects) = session.add_object(object.structure, object.source);
        info.object = name;
        apply_effects(&session, effects, viewport);
    }
    Ok(info)
}

/// Forwards a command's effects to the render thread.
pub(crate) fn apply_effects(session: &Session, effects: Vec<Effect>, viewport: &Viewport) {
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
fn save_session(path: String, viewport: State<'_, Viewport>, session: State<'_, Mutex<Session>>) -> Result<String, String> {
    save_to(&resolve_path(&path), "all", &viewport, &session.lock().unwrap())
}

/// `~/x` and relative paths resolve against the home folder (the app's working directory is
/// `/` when started from the Finder); a path without extension becomes a session.
fn resolve_path(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut p = match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(path).is_relative() => home.join(path),
        None => PathBuf::from(path),
    };
    if p.extension().is_none() {
        p.set_extension(rsmol_core::session_file::EXTENSION);
    }
    p
}

/// Writes a session (.rsmol) or the selected atoms (.pdb, .cif). Atomic: write, then rename.
pub(crate) fn save_to(path: &Path, selection: &str, viewport: &Viewport, session: &Session) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let bytes = if ext == rsmol_core::session_file::EXTENSION {
        if selection != "all" {
            return Err("sessions always hold everything; omit the selection".into());
        }
        let (reply, answer) = std::sync::mpsc::channel();
        viewport.send(Msg::GetCamera(reply))?;
        let camera = answer.recv_timeout(std::time::Duration::from_secs(1)).ok();
        session.save(camera)?
    } else {
        session.export(&ext, selection)?.into_bytes()
    };
    let partial = path.with_extension(format!("{ext}.partial"));
    std::fs::write(&partial, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))?;
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
    // `save` writes files, which is the app's job rather than the core session's.
    let line = text.trim_start();
    let word_end = line.find(char::is_whitespace).unwrap_or(line.len());
    if line[..word_end].eq_ignore_ascii_case("save") {
        let args = line[word_end..].trim();
        if args.is_empty() {
            let end = text.chars().count();
            return Err(SelectionErrorInfo { start: end, end, message: "save needs a file name (.rsmol, .pdb or .cif)".into() });
        }
        let (path, selection) = args.split_once(',').map_or((args.trim(), "all"), |(p, s)| (p.trim(), s.trim()));
        let resolved = resolve_path(path);
        let failed = |message: String| SelectionErrorInfo { start: 0, end: text.chars().count(), message };
        let message = if dry_run {
            let atoms = session.select_atoms(selection).map(|s| s.count()).unwrap_or(0);
            format!("save {atoms} atoms to {}", resolved.display())
        } else {
            save_to(&resolved, selection, &viewport, &session).map_err(failed)?
        };
        return Ok(state_result(&session, message, None, None));
    }
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

/// Runs a Python script file (`path`) or inline code against the live session.
#[tauri::command]
async fn run_python(path: Option<String>, code: Option<String>, app: AppHandle) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        let script = match (path, code) {
            (Some(path), _) => scripting::Script::File(resolve_script_path(&path)),
            (None, Some(code)) => scripting::Script::Code(code),
            (None, None) => return Err("nothing to run".into()),
        };
        let handle = app.clone();
        let result = tauri::async_runtime::spawn_blocking(move || scripting::run(handle, script))
            .await
            .map_err(|e| e.to_string())??;
        let _ = app.emit("session-changed", ());
        serde_json::to_value(result).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, code, app);
        Err("running scripts needs macOS or Linux for now".into())
    }
}

/// Like `resolve_path`, without adding an extension.
fn resolve_script_path(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(path).is_relative() => home.join(path),
        None => PathBuf::from(path),
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
            run_python, startup_file])
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
