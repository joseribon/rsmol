//! Running Python scripts against the live session.
//!
//! The app starts the user's Python with `RSMOL_APP_SOCKET` pointing at a Unix socket that
//! exists only for the duration of the run, inside a fresh directory only the user can read.
//! `rsmol.cmd` notices the variable and sends each call over the socket as one JSON line, so a
//! script drives the open window: the viewport and panels update as it runs. The pure-Python
//! `rsmol` package ships with the app and goes on `PYTHONPATH`, so no install is needed.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rsmol_core::session::{AtomField, Session};
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use crate::viewport::{CameraAction, Msg};
use crate::{Viewport, apply_effects};

/// What to run: a script file, or inline code from the command bar.
pub enum Script {
    File(PathBuf),
    Code(String),
}

#[derive(Serialize)]
pub struct RunResult {
    pub code: Option<i32>,
    /// Combined stdout and stderr, in arrival order.
    pub output: String,
}

/// `$RSMOL_PYTHON`, else the first Python 3 found in the usual places (apps started from the
/// Finder get a minimal PATH, so `python3` alone would miss Homebrew's).
fn interpreter() -> PathBuf {
    if let Some(p) = std::env::var_os("RSMOL_PYTHON") {
        return PathBuf::from(p);
    }
    ["/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("python3"))
}

/// The bundled `rsmol` Python package: the app's resources when installed, else the source tree.
fn python_path(app: &AppHandle) -> PathBuf {
    if let Ok(resources) = app.path().resource_dir() {
        let bundled = resources.join("python");
        if bundled.join("rsmol/cmd.py").exists() {
            return bundled;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/rsmol-py/python")
}

/// Runs the script to completion (call off the main thread) and returns its exit code and output.
pub fn run(app: AppHandle, script: Script) -> Result<RunResult, String> {
    let dir = std::env::temp_dir().join(format!(
        "rsmol-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    let result = run_in(&app, script, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_in(app: &AppHandle, script: Script, dir: &Path) -> Result<RunResult, String> {
    let socket = dir.join("app.sock");
    let listener = UnixListener::bind(&socket).map_err(|e| format!("could not open the script socket: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut command = Command::new(interpreter());
    let workdir = match &script {
        Script::File(path) => {
            command.arg(path);
            path.parent().map(Path::to_path_buf).unwrap_or(home)
        }
        Script::Code(code) => {
            // Inline code gets `cmd` pre-imported, as in PyMOL's command line.
            command.args(["-c", &format!("from rsmol import cmd\n{code}")]);
            home
        }
    };
    let mut pythonpath = python_path(app).into_os_string();
    if let Some(existing) = std::env::var_os("PYTHONPATH") {
        pythonpath.push(":");
        pythonpath.push(existing);
    }
    let mut child = command
        .current_dir(workdir)
        .env("RSMOL_APP_SOCKET", &socket)
        .env("PYTHONPATH", pythonpath)
        .env("PYTHONUNBUFFERED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", interpreter().display()))?;

    let output = Arc::new(Mutex::new(String::new()));
    let readers: Vec<_> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)]
        .into_iter()
        .flatten()
        .map(|stream| {
            let (output, app) = (output.clone(), app.clone());
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = app.emit("python-output", &line);
                    let mut out = output.lock().unwrap();
                    out.push_str(&line);
                    out.push('\n');
                }
            })
        })
        .collect();

    let code = loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let app = app.clone();
                std::thread::spawn(move || serve(&app, stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.to_string()),
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status.code();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    for reader in readers {
        let _ = reader.join();
    }
    let output = std::mem::take(&mut *output.lock().unwrap());
    Ok(RunResult { code, output })
}

/// Answers one connection: a JSON request per line, a JSON response per line.
fn serve(app: &AppHandle, stream: UnixStream) {
    let _ = stream.set_nonblocking(false);
    let Ok(mut writer) = stream.try_clone() else { return };
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                match dispatch(app, &method, &request["params"]) {
                    Ok(result) => json!({ "ok": true, "result": result }),
                    Err((kind, message)) => json!({ "ok": false, "kind": kind, "error": message }),
                }
            }
            Err(e) => json!({ "ok": false, "kind": "value", "error": format!("bad request: {e}") }),
        };
        if writeln!(writer, "{response}").is_err() {
            return;
        }
    }
}

type Failure = (&'static str, String);

fn value(message: impl Into<String>) -> Failure {
    ("value", message.into())
}

fn dispatch(app: &AppHandle, method: &str, params: &Value) -> Result<Value, Failure> {
    let session_state = app.state::<Mutex<Session>>();
    let viewport = app.state::<Viewport>();
    let text = |key: &str| params[key].as_str().unwrap_or_default().to_owned();
    let selection = || params["selection"].as_str().unwrap_or("all").to_owned();
    let changed = || {
        let _ = app.emit("session-changed", ());
    };

    let result = match method {
        "do" | "run" => {
            let command = text("command");
            let mut session = session_state.lock().unwrap();
            let outcome = session.run(&command, false).map_err(|e| value(e.render(&command)))?;
            apply_effects(&session, outcome.effects, &viewport);
            drop(session);
            changed();
            let mut d = json!({ "message": outcome.message, "atoms": outcome.atoms });
            for (key, v) in outcome.values {
                d[key] = json!(v);
            }
            if method == "do" { json!(d["message"]) } else { d }
        }
        "count_atoms" => {
            let sel = selection();
            json!(session_state.lock().unwrap().select_atoms(&sel).map_err(|e| value(e.render(&sel)))?.count())
        }
        "get_coords" => {
            let sel = selection();
            let atoms = session_state.lock().unwrap().atom_info(&sel).map_err(|e| value(e.render(&sel)))?;
            json!(atoms.iter().map(|a| [a.x, a.y, a.z]).collect::<Vec<_>>())
        }
        "get_atoms" => {
            let sel = selection();
            json!(session_state.lock().unwrap().atom_info(&sel).map_err(|e| value(e.render(&sel)))?)
        }
        "get_names" => json!(session_state.lock().unwrap().objects.iter().map(|o| o.name.clone()).collect::<Vec<_>>()),
        "get_measurements" => json!(session_state.lock().unwrap().measurement_info()),
        "alter" => {
            let changes: Vec<(usize, String, String)> =
                serde_json::from_value(params["changes"].clone()).map_err(|e| value(e.to_string()))?;
            let changes = changes
                .into_iter()
                .map(|(index, field, v)| {
                    let field = AtomField::from_name(&field).ok_or_else(|| value(format!("cannot alter '{field}'")))?;
                    Ok((index.checked_sub(1).ok_or_else(|| value("atom indices start at 1"))?, field, v))
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            let mut session = session_state.lock().unwrap();
            let effects = session.alter(&changes).map_err(value)?;
            apply_effects(&session, effects, &viewport);
            drop(session);
            changed();
            json!(changes.len())
        }
        "export" | "save" => {
            let path = PathBuf::from(text("path"));
            let sel = if method == "save" { "all".to_owned() } else { selection() };
            let session = session_state.lock().unwrap();
            json!(crate::save_to(&path, &sel, &viewport, &session).map_err(|e| ("io", e))?)
        }
        "load" | "fetch" => {
            let object = params["object"].as_str().map(str::to_owned);
            let path = if method == "load" {
                PathBuf::from(text("path"))
            } else {
                tauri::async_runtime::block_on(crate::fetch_to_cache(app, &text("code"))).map_err(|e| ("io", e))?
            };
            let info = tauri::async_runtime::block_on(crate::load(path, object, &viewport, &session_state)).map_err(|e| ("io", e))?;
            changed();
            json!(info.object)
        }
        "turn" => {
            let degrees = params["angle"].as_f64().unwrap_or(0.0) as f32;
            let axis = match text("axis").to_ascii_lowercase().as_str() {
                "x" => 'x',
                "y" => 'y',
                "z" => 'z',
                _ => return Err(value("axis must be 'x', 'y' or 'z'")),
            };
            viewport.send(Msg::Camera(CameraAction::Turn { axis, degrees })).map_err(|e| ("runtime", e))?;
            Value::Null
        }
        "reset" => {
            viewport.send(Msg::Camera(CameraAction::Reset)).map_err(|e| ("runtime", e))?;
            Value::Null
        }
        "png" => {
            return Err((
                "not_implemented",
                "png is not available inside the app yet; run the script with plain Python for figures".into(),
            ));
        }
        other => return Err(("not_implemented", format!("unknown method '{other}'"))),
    };
    Ok(result)
}
