//! The session behind the window: loading, fetching, saving and running commands. The UI and
//! Python scripts both go through here; everything visible is forwarded to the viewport.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rsmol_core::session::{Effect, Session, Source};
use rsmol_core::session_file::CameraState;

use crate::viewport::{Msg, Viewport};

/// The session and the viewport that shows it. Shared by the UI and script threads.
pub struct Backend {
    pub session: Mutex<Session>,
    pub viewport: Viewport,
}

#[derive(Clone, Debug)]
pub struct StructureInfo {
    pub name: String,
    /// The object name it was loaded as (made unique, or `name` for sessions).
    pub object: String,
    pub atoms: usize,
    pub states: usize,
    pub parse_ms: f64,
}

/// A command error, with character (not byte) offsets into the input for the carets.
#[derive(Clone, Debug)]
pub struct CommandError {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

impl CommandError {
    pub fn whole(message: impl Into<String>) -> Self {
        CommandError { start: 0, end: 0, message: message.into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionSummary {
    pub atoms: usize,
    pub residues: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementSummary {
    pub name: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSummary {
    pub name: String,
    pub atoms: usize,
    pub residues: usize,
    pub chains: Vec<String>,
    pub enabled: bool,
}

/// What the UI mirrors of the session after any change.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionState {
    /// The "sele" selection, for the selection card.
    pub selection: Option<SelectionSummary>,
    pub measurements: Vec<MeasurementSummary>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub objects: Vec<ObjectSummary>,
}

#[derive(Clone, Debug)]
pub struct CommandResult {
    pub message: String,
    pub canonical: Option<String>,
    pub atoms: Option<usize>,
}

/// The atom under the pointer, as the tooltip and measure tool show it.
#[derive(Clone, Debug)]
pub struct PickInfo {
    /// 1-based, as the `index` selection keyword expects.
    pub index: usize,
    pub label: String,
    pub chain: String,
    pub residue: String,
    pub atom: String,
    pub element: String,
    pub b_factor: f32,
}

impl Backend {
    pub fn new(viewport: Viewport) -> Self {
        Backend { session: Mutex::new(Session::default()), viewport }
    }

    pub fn state(&self) -> SessionState {
        state_of(&self.session.lock().unwrap())
    }

    /// Loads a structure file as a new object, or replaces everything with a `.rsmol` session
    /// (display, selections, measurements and camera included). `object` names the new object.
    /// Blocks while reading and parsing; call it off the UI thread.
    pub fn load(&self, path: &Path, object: Option<String>) -> Result<StructureInfo, String> {
        let start = Instant::now();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
        let is_session = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(rsmol_core::session_file::EXTENSION));
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (loaded, camera) = if is_session {
            Session::open(&bytes)?
        } else {
            let mut structure = rsmol_core::io::parse_bytes(&bytes, &file_name).map_err(|e| e.to_string())?;
            if let Some(name) = object {
                structure.name = name;
            }
            let mut s = Session::default();
            s.load_with_source(Arc::new(structure), Source { file_name, data: Arc::new(bytes) });
            (s, None)
        };

        let structure =
            if is_session { loaded.structure.clone().expect("sessions hold structures") } else { loaded.objects[0].structure.clone() };
        let mut info = StructureInfo {
            name: structure.name.clone(),
            object: structure.name.clone(),
            atoms: structure.atom_count(),
            states: structure.state_count(),
            parse_ms: start.elapsed().as_secs_f64() * 1e3,
        };
        let mut session = self.session.lock().unwrap();
        if is_session {
            *session = loaded;
            let highlight = session.named.get("sele").map(|set| set.iter().collect()).unwrap_or_default();
            let effects = vec![Effect::Scene { refit: true }, Effect::Measurements, Effect::Highlight(highlight)];
            self.apply_effects(&session, effects);
            if let Some(camera) = camera {
                self.viewport.send(Msg::SetCamera(camera));
            }
        } else {
            let object = loaded.objects.into_iter().next().expect("loading made one object");
            let (name, effects) = session.add_object(object.structure, object.source);
            info.object = name;
            self.apply_effects(&session, effects);
        }
        Ok(info)
    }

    /// Forwards a command's effects to the viewport.
    pub fn apply_effects(&self, session: &Session, effects: Vec<Effect>) {
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
            self.viewport.send(msg);
        }
    }

    /// Runs a command line. With `dry_run`, only reports what it would do, for live feedback.
    /// `save` is handled here: writing files is the app's job rather than the core session's.
    pub fn run_command(&self, text: &str, dry_run: bool) -> Result<CommandResult, CommandError> {
        let mut session = self.session.lock().unwrap();
        let line = text.trim_start();
        let word_end = line.find(char::is_whitespace).unwrap_or(line.len());
        if line[..word_end].eq_ignore_ascii_case("save") {
            let args = line[word_end..].trim();
            let end = text.chars().count();
            if args.is_empty() {
                return Err(CommandError { start: end, end, message: "save needs a file name (.rsmol, .pdb or .cif)".into() });
            }
            let (path, selection) = args.split_once(',').map_or((args.trim(), "all"), |(p, s)| (p.trim(), s.trim()));
            let resolved = resolve_path(path, true);
            let message = if dry_run {
                let atoms = session.select_atoms(selection).map(|s| s.count()).unwrap_or(0);
                format!("save {atoms} atoms to {}", resolved.display())
            } else {
                self.save_to(&resolved, selection, &session).map_err(|message| CommandError { start: 0, end, message })?
            };
            return Ok(CommandResult { message, canonical: None, atoms: None });
        }
        let outcome = session.run(text, dry_run).map_err(|e| CommandError {
            start: text[..e.span.start].chars().count(),
            end: text[..e.span.end].chars().count(),
            message: e.message,
        })?;
        self.apply_effects(&session, outcome.effects);
        Ok(CommandResult { message: outcome.message, canonical: outcome.canonical, atoms: outcome.atoms })
    }

    /// Saves the session (structure, display, selections, measurements, camera) to `path`.
    pub fn save_session(&self, path: &Path) -> Result<String, String> {
        self.save_to(path, "all", &self.session.lock().unwrap())
    }

    /// Writes a session (.rsmol) or the selected atoms (.pdb, .cif). Atomic: write, then rename.
    pub fn save_to(&self, path: &Path, selection: &str, session: &Session) -> Result<String, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
        let bytes = if ext == rsmol_core::session_file::EXTENSION {
            if selection != "all" {
                return Err("sessions always hold everything; omit the selection".into());
            }
            session.save(self.camera())?
        } else {
            session.export(&ext, selection)?.into_bytes()
        };
        let partial = path.with_extension(format!("{ext}.partial"));
        std::fs::write(&partial, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(format!("saved {} ({:.1} MB)", path.display(), bytes.len() as f64 / 1e6))
    }

    fn camera(&self) -> Option<CameraState> {
        let (reply, answer) = std::sync::mpsc::channel();
        self.viewport.send(Msg::GetCamera(reply));
        answer.recv_timeout(Duration::from_secs(1)).ok()
    }

    /// The atom drawn at physical pixel (`x`, `y`), if any. Blocks until the viewport answers.
    pub fn pick(&self, x: f32, y: f32) -> Option<PickInfo> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let (reply, answer) = std::sync::mpsc::channel();
        self.viewport.send(Msg::Pick { x: x as u32, y: y as u32, reply });
        let atom = answer.recv_timeout(Duration::from_millis(250)).ok().flatten()?;
        pick_info(&self.session.lock().unwrap(), atom)
    }
}

pub fn state_of(session: &Session) -> SessionState {
    let selection = session.named.get("sele").zip(session.structure.as_ref()).map(|(set, s)| SelectionSummary {
        atoms: set.count(),
        residues: set.iter().map(|i| s.atoms.residue[i]).collect::<BTreeSet<_>>().len(),
    });
    SessionState {
        selection,
        measurements: session.measurements.iter().map(|m| MeasurementSummary { name: m.name.clone(), label: m.label() }).collect(),
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

fn pick_info(session: &Session, i: usize) -> Option<PickInfo> {
    let s = session.structure.as_ref().filter(|s| i < s.atom_count())?;
    let r = s.residue_of(i);
    Some(PickInfo {
        index: i + 1,
        label: s.atom_label(i),
        chain: s.chain_of(i).id.clone(),
        residue: format!("{} {}{}", r.name, r.seq_num, r.ins_code.map(String::from).unwrap_or_default()),
        atom: s.atoms.name[i].clone(),
        element: s.atoms.element[i].symbol().to_owned(),
        b_factor: s.atoms.b_factor[i],
    })
}

/// `~/x` and relative paths resolve against the home folder (an app started from the Finder
/// or a desktop launcher has `/` as working directory). With `session_default`, a path without
/// extension becomes a session.
pub fn resolve_path(path: &str, session_default: bool) -> PathBuf {
    let home = home_dir();
    let mut p = match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(path).is_relative() => home.join(path),
        None => PathBuf::from(path),
    };
    if session_default && p.extension().is_none() {
        p.set_extension(rsmol_core::session_file::EXTENSION);
    }
    p
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

/// Where downloaded PDB entries are kept. The same folder as the earlier Tauri app, so its
/// downloads are reused.
fn pdb_cache() -> Result<PathBuf, String> {
    Ok(dirs::cache_dir().ok_or("no cache folder on this system")?.join("dev.rsmol.app").join("pdb"))
}

/// Downloads `<ID>.cif.gz` from the RCSB PDB into the cache, unless already there. Blocks.
pub fn fetch_to_cache(id: &str) -> Result<PathBuf, String> {
    let id = id.trim().to_ascii_uppercase();
    if !is_pdb_id(&id) {
        return Err(format!("'{id}' is not a PDB ID (four characters starting with a digit, e.g. 1HSG)"));
    }
    let cache = pdb_cache()?;
    let path = cache.join(format!("{id}.cif.gz"));
    if !path.exists() {
        let url = format!("https://files.rcsb.org/download/{id}.cif.gz");
        let mut response = ureq::get(&url).call().map_err(|e| match e {
            ureq::Error::StatusCode(404) => format!("{id} was not found at the RCSB PDB"),
            e => format!("download failed: {e}"),
        })?;
        // Large assemblies (ribosomes) exceed ureq's 10 MB default.
        let bytes = response.body_mut().with_config().limit(512 * 1024 * 1024).read_to_vec().map_err(|e| e.to_string())?;
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
