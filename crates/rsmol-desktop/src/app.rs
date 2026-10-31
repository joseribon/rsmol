//! The window's contents: the molecule, and the panels floating over it.
//!
//! Every action (command bar, buttons, clicks in the viewport, dropped files) goes through
//! `execute`, so it lands in one history that shows the PyMOL syntax behind it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use rsmol_core::session::PRESETS;
use serde::{Deserialize, Serialize};
use wgpui::{
    Animation, AnimationExt, AnyElement, App, Context, CursorStyle, DispatchPhase, Entity, ExternalPaths, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    PathPromptOptions, Pixels, Point, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, SharedString, Styled, Transformation,
    WgpuSurfaceHandle, Window, actions, canvas, div, percentage, prelude::*, px, rgba, svg, wgpu_surface,
};

use wgpui::Focusable;

use crate::backend::{self, Backend, CommandError, CommandResult, PickInfo, SessionState, StructureInfo};
use crate::text_input::{InputEvent, TextInput};
use crate::theme::{self, fmt, panel};
use crate::viewport::{CameraAction, Label, Msg, ViewEvent};

actions!(rsmol, [OpenFile, SaveFile, Quit]);

type Outcome = Result<CommandResult, CommandError>;

/// One line of the command history.
struct Entry {
    input: String,
    output: String,
    ok: bool,
}

/// Something that can be reopened from the start screen.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Recent {
    File { path: PathBuf, name: String },
    Fetch { id: String },
}

impl Recent {
    fn label(&self) -> &str {
        match self {
            Recent::File { name, .. } => name,
            Recent::Fetch { id } => id,
        }
    }
}

/// What outlives a run: recent files and the history toggle.
#[derive(Default, Serialize, Deserialize)]
struct Prefs {
    #[serde(default)]
    recent: Vec<Recent>,
    #[serde(default)]
    history_expanded: bool,
}

impl Prefs {
    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("rsmol").join("prefs.json"))
    }

    fn load() -> Prefs {
        Self::path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        let _ = path.parent().map(std::fs::create_dir_all);
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            // Storage unavailable: preferences only last for this run.
            let _ = std::fs::write(path, json);
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum DragMode {
    Orbit,
    Pan,
}

struct Drag {
    mode: DragMode,
    button: MouseButton,
    last: Point<Pixels>,
    /// Pointer travel since the button went down; below CLICK_SLOP a release is a click.
    travel: f32,
}

/// How `execute` reports and logs a line.
#[derive(Clone, Copy, Default)]
struct Exec {
    /// Only report what the line would do.
    dry_run: bool,
    /// Log failures in the history (button clicks); the command bar shows them inline instead.
    quiet_errors: bool,
    /// Typed in the command bar: clear it on success, show the error under it otherwise.
    from_bar: bool,
}

const HISTORY_LIMIT: usize = 5;
const RECENT_LIMIT: usize = 6;
/// Lines of script output kept in the history entry.
const OUTPUT_LINES: usize = 30;
const CLICK_SLOP: f32 = 4.0;
const STRUCTURE_EXTENSIONS: &[&str] = &["cif", "mmcif", "pdb", "ent", "gz", "rsmol", "py"];
const EXAMPLES: &[(&str, &str)] =
    &[("1HSG", "HIV protease with indinavir"), ("4HHB", "Hemoglobin"), ("1AON", "GroEL–GroES chaperonin"), ("6VXX", "SARS-CoV-2 spike")];
/// Swatch colors; names and values mirror rsmol-core's color.rs, so a swatch looks exactly like
/// what `color <name>` produces.
const SWATCHES: &[(&str, u32)] = &[
    ("teal", 0x60baa8),
    ("sky", 0x76b0d6),
    ("lavender", 0xb4a0e6),
    ("rose", 0xe26e8c),
    ("coral", 0xf0806e),
    ("amber", 0xf5b43c),
    ("mint", 0x8cd6b4),
    ("wheat", 0xecd4a0),
    ("slate", 0x7080a0),
    ("white", 0xeceef2),
];
const SCHEMES: &[(&str, &str, &str)] = &[
    ("Chain", "bychain", "One color per chain"),
    ("Element", "byelement", "Heteroatoms by element, carbons unchanged"),
    ("Structure", "byss", "Helix, sheet and loop"),
    ("B-factor", "bfactor", "Blue (ordered) to red (mobile)"),
    ("Default", "default", "Chain carbons, element heteroatoms"),
];
const SELECTION_ACTIONS: &[(&str, &str)] = &[
    ("Zoom", "zoom sele"),
    ("Sticks", "show sticks, sele"),
    ("Spheres", "show spheres, sele"),
    ("Cartoon", "show cartoon, sele"),
    ("Surface", "show surface, sele"),
    ("Hide", "hide everything, sele"),
];

pub struct RsmolApp {
    backend: Arc<Backend>,
    surface: WgpuSurfaceHandle,
    focus: FocusHandle,
    input: Entity<TextInput>,
    script_events: UnboundedSender<crate::scripting::ScriptEvent>,

    /// The session as the panels show it.
    state: SessionState,
    /// The first structure's details; `None` shows the start screen.
    info: Option<StructureInfo>,
    loading: bool,
    error: Option<String>,
    history: Vec<Entry>,
    history_scroll: ScrollHandle,
    history_index: Option<usize>,
    /// Live feedback for the command bar text.
    result: Option<Outcome>,
    prefs: Prefs,
    /// The atom under the pointer and where (window coordinates).
    hover: Option<(PickInfo, Point<Pixels>)>,
    hover_busy: bool,
    hover_next: Option<Point<Pixels>>,
    /// Measure tool: atoms picked so far, or `None` when the tool is off.
    measuring: Option<Vec<PickInfo>>,
    /// While a Python script runs: its latest output line ("" until it prints).
    running: Option<String>,
    labels: Vec<Label>,
    stats: Option<(f64, String)>,
    active_look: usize,
    drag: Option<Drag>,
    /// Physical pixels per logical pixel, as of the last frame.
    scale: f32,
    /// Frames to keep requesting after the window opens: WGPUI can drop the first ones while
    /// the window is still appearing, and nothing else would trigger a repaint.
    warmup: u32,
}

impl RsmolApp {
    pub fn new(
        backend: Arc<Backend>,
        surface: WgpuSurfaceHandle,
        view_events: UnboundedReceiver<ViewEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("byres (organic around 5) and polymer   ·   show sticks, sele   ·   run script.py", cx));
        cx.subscribe(&input, Self::on_input_event).detach();

        cx.spawn(async move |this, cx| {
            let mut events = view_events;
            while let Some(event) = events.next().await {
                if this.update(cx, |this, cx| this.on_view_event(event, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let (script_events, mut script_rx) = futures::channel::mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while let Some(event) = script_rx.next().await {
                let alive = this.update(cx, |this, cx| {
                    match event {
                        crate::scripting::ScriptEvent::SessionChanged => this.refresh_state(),
                        crate::scripting::ScriptEvent::Output(line) => {
                            if this.running.is_some() {
                                this.running = Some(line);
                            }
                        }
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();

        // Back in front: repaint, in case frames were dropped while hidden.
        cx.observe_window_activation(window, |this, window, _| {
            this.warmup = this.warmup.max(10);
            window.refresh();
        })
        .detach();

        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        RsmolApp {
            backend,
            surface,
            focus,
            input,
            script_events,
            state: SessionState::default(),
            info: None,
            loading: false,
            error: None,
            history: Vec::new(),
            history_scroll: ScrollHandle::new(),
            history_index: None,
            result: None,
            prefs: Prefs::load(),
            hover: None,
            hover_busy: false,
            hover_next: None,
            measuring: None,
            running: None,
            labels: Vec::new(),
            stats: None,
            active_look: 0,
            drag: None,
            scale: window.scale_factor(),
            warmup: 60,
        }
    }

    // --- commands --------------------------------------------------------------------------

    /// Runs `input`. Downloads and scripts finish later, so they return `None` and report
    /// through the history (and the command bar, for lines typed there).
    fn execute(&mut self, input: &str, exec: Exec, cx: &mut Context<Self>) -> Option<Outcome> {
        let line = input.trim();
        if line.is_empty() {
            return None;
        }
        let (word, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(w, r)| (w, r.trim()));
        let word = word.to_ascii_lowercase();
        if (word == "run" || word == "python") && !rest.is_empty() {
            // Scripts run in Python, outside the core session.
            if exec.dry_run {
                let what = if word == "run" { format!("run {rest} with Python") } else { "run Python code".into() };
                return Some(Ok(message(what)));
            }
            let script = if word == "run" {
                crate::scripting::Script::File(backend::resolve_path(rest, false))
            } else {
                crate::scripting::Script::Code(rest.to_owned())
            };
            self.run_script(input.to_owned(), script, exec, cx);
            return None;
        }
        if word == "fetch" && !rest.is_empty() && !rest.contains(char::is_whitespace) {
            // Downloading happens in the app, not the core session.
            let id = rest.to_ascii_uppercase();
            if exec.dry_run {
                return Some(Ok(message(format!("fetch {id} from the RCSB PDB"))));
            }
            self.fetch(input.to_owned(), id, exec, cx);
            return None;
        }
        let outcome = self.backend.run_command(input, exec.dry_run);
        if !exec.dry_run {
            self.finish(input.to_owned(), outcome.clone(), exec, cx);
        }
        Some(outcome)
    }

    /// Logs a finished line and mirrors the session.
    fn finish(&mut self, input: String, outcome: Outcome, exec: Exec, cx: &mut Context<Self>) {
        self.refresh_state();
        match &outcome {
            Ok(r) => self.log(input.clone(), r.message.clone(), true),
            Err(e) if !exec.quiet_errors => self.log(input.clone(), e.message.clone(), false),
            Err(_) => {}
        }
        if exec.from_bar {
            if outcome.is_ok() {
                if self.input.read(cx).text() == input {
                    self.input.update(cx, |input, cx| input.set_text("", cx));
                }
                self.result = None;
            } else {
                self.result = Some(outcome);
            }
        }
        cx.notify();
    }

    fn run_script(&mut self, input: String, script: crate::scripting::Script, exec: Exec, cx: &mut Context<Self>) {
        self.running = Some(String::new());
        cx.notify();
        let (backend, events) = (self.backend.clone(), self.script_events.clone());
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { run_python(backend, script, events) }).await;
            this.update(cx, |this, cx| {
                this.running = None;
                this.finish(input, result, exec, cx);
            })
            .ok();
        })
        .detach();
    }

    fn fetch(&mut self, input: String, id: String, exec: Exec, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let fetch_id = id.clone();
        self.load_with(
            move || backend::fetch_to_cache(&fetch_id).and_then(|path| backend.load(&path, None)),
            move |this, loaded, cx| {
                let outcome = match loaded {
                    Ok(info) => {
                        this.remember(Recent::Fetch { id });
                        Ok(message(format!("{}: {} atoms in {:.0} ms", info.name, fmt(info.atoms), info.parse_ms)))
                    }
                    // The error goes to the history or the command bar, not a toast.
                    Err(e) => {
                        this.error = None;
                        Err(CommandError::whole(e))
                    }
                };
                this.finish(input, outcome, exec, cx);
            },
            cx,
        );
    }

    /// Loads a structure or `.rsmol` session (dropped, chosen, from the command line, recent),
    /// or runs a `.py` script.
    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("py")) {
            self.execute(&format!("run {}", path.display()), Exec::default(), cx);
            return;
        }
        let backend = self.backend.clone();
        let load_path = path.clone();
        self.load_with(
            move || backend.load(&load_path, None),
            move |this, loaded, _cx| {
                let Ok(info) = loaded else { return };
                let is_session = path.extension().is_some_and(|e| e.eq_ignore_ascii_case(rsmol_core::session_file::EXTENSION));
                let name = if is_session {
                    path.file_name().map_or(info.name.clone(), |n| n.to_string_lossy().into_owned())
                } else {
                    info.name.clone()
                };
                this.remember(Recent::File { path: path.clone(), name: name.clone() });
                this.refresh_state();
                let verb = if is_session { "open" } else { "load" };
                this.log(format!("{verb} {name}"), format!("{} atoms", fmt(info.atoms)), true);
            },
            cx,
        );
    }

    /// Shows the loading screen while `load` runs in the background, then hands the result to
    /// `done`. Failures also show as a toast.
    fn load_with(
        &mut self,
        load: impl FnOnce() -> Result<StructureInfo, String> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<StructureInfo, String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.loading = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let loaded = cx.background_spawn(async move { load() }).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match &loaded {
                    Ok(info) => {
                        this.info = Some(info.clone());
                        this.active_look = 0;
                        if this.measuring.is_some() {
                            this.measuring = Some(Vec::new());
                        }
                    }
                    Err(e) => this.error = Some(e.clone()),
                }
                done(this, loaded, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open a structure, session or script".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.open_file(path, cx)).ok();
            }
        })
        .detach();
    }

    /// Asks where to save, then writes the session there (or a structure file, by extension).
    fn choose_save_location(&mut self, cx: &mut Context<Self>) {
        if self.info.is_none() {
            return;
        }
        let name = self.state.objects.first().map_or("session".to_owned(), |o| o.name.clone());
        let path = cx.prompt_for_new_path(&backend::home_dir(), Some(&format!("{name}.{}", rsmol_core::session_file::EXTENSION)));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else { return };
            this.update(cx, |this, cx| this.save_to(path, cx)).ok();
        })
        .detach();
    }

    fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path.extension().is_some_and(|e| e.eq_ignore_ascii_case(rsmol_core::session_file::EXTENSION)) {
            match self.backend.save_session(&path) {
                Ok(message) => {
                    self.log("save session".into(), message, true);
                    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                    self.remember(Recent::File { path, name });
                }
                Err(e) => self.log("save session".into(), e, false),
            }
            cx.notify();
        } else {
            // Structure formats go through the command, so the history shows how to script it.
            self.execute(&format!("save {}", path.display()), Exec::default(), cx);
        }
    }

    fn reopen(&mut self, recent: Recent, cx: &mut Context<Self>) {
        match recent {
            Recent::File { path, .. } => self.open_file(path, cx),
            Recent::Fetch { id } => {
                self.execute(&format!("fetch {id}"), Exec::default(), cx);
            }
        }
    }

    fn apply_look(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(Ok(_)) = self.execute(PRESETS[index].1, Exec::default(), cx) {
            self.active_look = index;
        }
    }

    /// Re-reads what the panels show from the session.
    fn refresh_state(&mut self) {
        self.state = self.backend.state();
        if self.state.objects.is_empty() {
            // Everything deleted: back to the start screen.
            self.info = None;
        } else if self.info.is_none() {
            // A script loaded the first structure: leave the start screen.
            let o = &self.state.objects[0];
            self.info = Some(StructureInfo { name: o.name.clone(), object: o.name.clone(), atoms: o.atoms, states: 1, parse_ms: 0.0 });
        }
    }

    fn log(&mut self, input: String, output: String, ok: bool) {
        if self.history.len() >= HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(Entry { input, output, ok });
        self.history_scroll.scroll_to_bottom();
    }

    fn remember(&mut self, recent: Recent) {
        self.prefs.recent.retain(|r| r != &recent && r.label() != recent.label());
        self.prefs.recent.insert(0, recent);
        self.prefs.recent.truncate(RECENT_LIMIT);
        self.prefs.save();
    }

    // --- command bar -----------------------------------------------------------------------

    fn on_input_event(&mut self, input: Entity<TextInput>, event: &InputEvent, cx: &mut Context<Self>) {
        let text = input.read(cx).text().to_owned();
        match event {
            InputEvent::Changed => {
                self.result = if text.trim().is_empty() { None } else { self.execute(&text, Exec { dry_run: true, ..Exec::default() }, cx) };
            }
            InputEvent::Submit => {
                self.history_index = None;
                if text.trim().is_empty() {
                    return;
                }
                // Errors stay inline (the text is kept so it can be fixed in place).
                self.execute(&text, Exec { quiet_errors: true, from_bar: true, ..Exec::default() }, cx);
            }
            InputEvent::HistoryUp => {
                if self.history.is_empty() {
                    return;
                }
                let index = self.history_index.map_or(self.history.len() - 1, |i| i.saturating_sub(1));
                self.history_index = Some(index);
                let text = self.history[index].input.clone();
                self.input.update(cx, |input, cx| input.set_text(text, cx));
            }
            InputEvent::Cancel => {
                self.input.update(cx, |input, cx| input.set_text("", cx));
                self.result = None;
            }
        }
        cx.notify();
    }

    fn toggle_history(&mut self, cx: &mut Context<Self>) {
        self.prefs.history_expanded = !self.prefs.history_expanded;
        self.prefs.save();
        self.history_scroll.scroll_to_bottom();
        cx.notify();
    }

    // --- viewport input --------------------------------------------------------------------

    fn on_view_event(&mut self, event: ViewEvent, cx: &mut Context<Self>) {
        match event {
            ViewEvent::Labels(labels) => self.labels = labels,
            ViewEvent::Stats { frame_ms, gpu } => self.stats = Some((frame_ms, gpu)),
        }
        cx.notify();
    }

    fn camera(&self, action: CameraAction) {
        self.backend.viewport.send(Msg::Camera(action));
    }

    fn on_viewport_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.hover = None;
        if event.click_count == 2 && event.button == MouseButton::Left {
            self.drag = None;
            // Two quick picks while measuring are not a request to zoom.
            if self.measuring.is_none() {
                self.pick_then(event.position, cx, |this, info, cx| match info {
                    Some(info) => {
                        this.execute(&format!("zoom byres index {}", info.index), Exec::default(), cx);
                    }
                    None => this.camera(CameraAction::Reset),
                });
            }
            return;
        }
        let mode = if event.button == MouseButton::Left && !event.modifiers.shift { DragMode::Orbit } else { DragMode::Pan };
        self.drag = Some(Drag { mode, button: event.button, last: event.position, travel: 0.0 });
        cx.notify();
    }

    /// Mouse moves anywhere in the window: drags keep going over panels and outside.
    fn on_window_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &mut self.drag else { return };
        let delta = event.position - drag.last;
        drag.last = event.position;
        drag.travel += f32::from(delta.x).abs() + f32::from(delta.y).abs();
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let action = match drag.mode {
            DragMode::Orbit => CameraAction::Orbit { dx, dy },
            DragMode::Pan => CameraAction::Pan { dx, dy, height: f32::from(window.viewport_size().height) },
        };
        self.camera(action);
        cx.notify();
    }

    fn on_window_mouse_up(&mut self, event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if drag.button != event.button {
            self.drag = Some(drag);
            return;
        }
        cx.notify();
        if drag.travel >= CLICK_SLOP || event.button != MouseButton::Left {
            return;
        }
        let shift = event.modifiers.shift;
        self.pick_then(event.position, cx, move |this, info, cx| {
            let Some(info) = info else { return };
            if let Some(picked) = &mut this.measuring {
                // Every second atom completes a distance.
                picked.push(info);
                if picked.len() == 2 {
                    let command = format!("distance index {}, index {}", picked[0].index, picked[1].index);
                    this.measuring = Some(Vec::new());
                    this.execute(&command, Exec::default(), cx);
                }
                cx.notify();
                return;
            }
            // Shift-click extends the selection; a plain click replaces it.
            let residue = format!("byres index {}", info.index);
            let command = if shift && this.state.selection.is_some() { format!("select sele or {residue}") } else { format!("select {residue}") };
            this.execute(&command, Exec::default(), cx);
        });
    }

    fn on_viewport_hover(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() || event.pressed_button.is_some() {
            return;
        }
        self.hover_at(event.position, cx);
    }

    /// At most one hover pick in flight; the latest position wins.
    fn hover_at(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.hover_busy {
            self.hover_next = Some(position);
            return;
        }
        self.hover_busy = true;
        self.pick_then(position, cx, move |this, info, cx| {
            this.hover_busy = false;
            // Dragging started or the pointer moved on meanwhile: drop the stale result.
            if this.drag.is_none() && this.hover_next.is_none() {
                this.hover = info.map(|info| (info, position));
            }
            if let Some(next) = this.hover_next.take() {
                this.hover_busy = true;
                this.pick_then(next, cx, move |this, info, cx| {
                    this.hover_busy = false;
                    if this.drag.is_none() {
                        this.hover = info.map(|info| (info, next));
                    }
                    cx.notify();
                });
            }
            cx.notify();
        });
    }

    /// Picks the atom at a window position off the UI thread, then calls `then` with it.
    fn pick_then(&mut self, position: Point<Pixels>, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Option<PickInfo>, &mut Context<Self>) + 'static) {
        let (x, y) = (f32::from(position.x) * self.scale, f32::from(position.y) * self.scale);
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let info = cx.background_spawn(async move { backend.pick(x, y) }).await;
            this.update(cx, |this, cx| then(this, info, cx)).ok();
        })
        .detach();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = match event.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y),
            ScrollDelta::Lines(l) => l.y * 20.0,
        };
        self.hover = None;
        self.camera(CameraAction::Zoom { factor: (-dy * 0.002).exp() });
        cx.notify();
    }

    /// M toggles the measure tool and Escape leaves it; ⌘Z / ⇧⌘Z undo and redo. Not while the
    /// command bar has focus, where keys edit the text.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).focus_handle(cx).is_focused(window) || self.info.is_none() {
            return;
        }
        let k = &event.keystroke;
        let command = k.modifiers.platform || k.modifiers.control;
        match k.key.as_str() {
            "z" if command => {
                let line = if k.modifiers.shift { "redo" } else { "undo" };
                self.execute(line, Exec { quiet_errors: true, ..Exec::default() }, cx);
            }
            _ if command => return,
            "m" => self.measuring = if self.measuring.is_some() { None } else { Some(Vec::new()) },
            "escape" if self.measuring.is_some() => self.measuring = None,
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn message(message: String) -> CommandResult {
    CommandResult { message, canonical: None, atoms: None }
}

fn run_python(backend: Arc<Backend>, script: crate::scripting::Script, events: UnboundedSender<crate::scripting::ScriptEvent>) -> Outcome {
    #[cfg(unix)]
    {
        match crate::scripting::run(backend, script, events) {
            Ok(run) => {
                let lines: Vec<&str> = run.output.trim_end().lines().collect();
                let tail = if lines.len() > OUTPUT_LINES {
                    std::iter::once("…").chain(lines[lines.len() - OUTPUT_LINES..].iter().copied()).collect::<Vec<_>>().join("\n")
                } else {
                    lines.join("\n")
                };
                if run.code == Some(0) {
                    Ok(message(if tail.is_empty() { "done".into() } else { tail }))
                } else {
                    let code = run.code.map_or("a signal".into(), |c| format!("code {c}"));
                    Err(CommandError::whole(if tail.is_empty() { format!("Python exited with {code}") } else { tail }))
                }
            }
            Err(e) => Err(CommandError::whole(e)),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (backend, script, events);
        Err(CommandError::whole("running scripts needs macOS or Linux for now"))
    }
}

// --- rendering ------------------------------------------------------------------------------

/// A small bordered button, as used for schemes, presets and actions.
fn chip(id: impl Into<SharedString>, label: impl Into<SharedString>) -> wgpui::Stateful<wgpui::Div> {
    div()
        .id(id.into())
        .px(px(8.0))
        .py(px(3.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::border())
        .bg(theme::white_a(0x0a))
        .text_color(theme::muted())
        .text_size(px(12.0))
        .cursor_pointer()
        .hover(|s| s.text_color(theme::text()).bg(theme::white_a(0x17)))
        .child(label.into())
}

fn icon(path: &'static str, size: f32) -> wgpui::Svg {
    svg().path(path).size(px(size)).flex_none()
}

impl RsmolApp {
    fn render_viewport(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let cursor = match (&self.drag, &self.hover, &self.measuring) {
            (Some(_), _, _) => CursorStyle::ClosedHand,
            (None, Some(_), Some(_)) => CursorStyle::Crosshair,
            (None, Some(_), None) => CursorStyle::PointingHand,
            _ => CursorStyle::OpenHand,
        };
        let backend = self.backend.clone();
        let this = cx.entity().downgrade();
        div()
            .id("viewport")
            .absolute()
            .inset_0()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_viewport_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_viewport_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_viewport_down))
            .on_mouse_move(cx.listener(Self::on_viewport_hover))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered {
                    this.hover_next = None;
                    this.hover = None;
                    cx.notify();
                }
            }))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                wgpu_surface(self.surface.clone())
                    .absolute()
                    .inset_0()
                    .on_resize(move |_, _, _| backend.viewport.send(Msg::Resized)),
            )
            // Window-wide listeners, so a drag continues over the panels and outside the window.
            .child(canvas(|_, _, _| (), move |_, _, window, _| {
                let moves = this.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        moves.update(cx, |this, cx| this.on_window_mouse_move(e, window, cx)).ok();
                    }
                });
                let ups = this.clone();
                window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        ups.update(cx, |this, cx| this.on_window_mouse_up(e, window, cx)).ok();
                    }
                });
            }).absolute().size_0())
    }

    fn render_labels(&self, window: &Window) -> impl IntoElement {
        let scale = window.scale_factor();
        div().absolute().inset_0().children(self.labels.iter().filter_map(|label| {
            let [x, y] = label.position?;
            // A zero-size box at the anchor, with the pill centered on it.
            Some(
                div().absolute().left(px(x / scale)).top(px(y / scale)).size_0().flex().items_center().justify_center().child(
                    div()
                        .flex_none()
                        .px(px(6.0))
                        .py(px(1.0))
                        .rounded_full()
                        .bg(rgba(0xf5c454eb))
                        .text_color(rgba(0x1a1406ff))
                        .text_size(px(11.0))
                        .font_family(theme::MONO)
                        .font_weight(FontWeight::SEMIBOLD)
                        .shadow_md()
                        .whitespace_nowrap()
                        .child(label.text.clone()),
                ),
            )
        }))
    }

    fn render_tooltip(&self) -> Option<impl IntoElement> {
        let (info, at) = self.hover.as_ref()?;
        Some(
            panel()
                .absolute()
                .left(at.x + px(14.0))
                .top(at.y + px(14.0))
                .rounded(px(9.0))
                .px(px(10.0))
                .py(px(7.0))
                .whitespace_nowrap()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(
                            div()
                                .px(px(5.0))
                                .rounded(px(4.0))
                                .bg(theme::accent_a(0x2e))
                                .text_color(theme::accent())
                                .text_size(px(11.0))
                                .font_family(theme::MONO)
                                .child(if info.chain.is_empty() { "–".to_owned() } else { info.chain.clone() }),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(info.residue.clone())),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .text_size(px(12.0))
                        .font_family(theme::MONO)
                        .text_color(theme::muted())
                        .child(format!("{} · {} · B {:.1}", info.atom, info.element, info.b_factor)),
                )
                .child(div().mt(px(4.0)).text_size(px(10.5)).text_color(theme::faint()).child("click to select · shift-click to add · double-click to zoom")),
        )
    }

    fn render_titlebar(&self, window: &Window) -> impl IntoElement {
        let macos = cfg!(target_os = "macos");
        let names: Vec<_> = self.state.objects.iter().map(|o| o.name.clone()).collect();
        div()
            .id("titlebar")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(38.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            // Clear the macOS traffic lights, which float over the content.
            .pl(px(if macos && !window.is_fullscreen() { 84.0 } else { 16.0 }))
            .when(macos, |d| {
                d.on_mouse_down(MouseButton::Left, |e, window, _| {
                    if e.click_count == 2 {
                        window.zoom_window();
                    } else {
                        window.start_window_move();
                    }
                })
            })
            .child(div().font_weight(FontWeight::SEMIBOLD).child("rsmol"))
            .when(self.info.is_some(), |d| {
                d.child(div().text_color(theme::faint()).child("/")).child(div().text_color(theme::muted()).child(names.join(" + ")))
            })
    }

    fn render_tools(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let measuring = self.measuring.is_some();
        let undo_redo = self.state.can_undo || self.state.can_redo;
        let tool = |id: &'static str| {
            panel().id(id).flex().items_center().gap(px(6.0)).px(px(10.0)).py(px(6.0)).text_size(px(12.0)).cursor_pointer()
        };
        let small = |id: &'static str, path: &'static str, enabled: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .w(px(28.0))
                .h(px(26.0))
                .rounded(px(7.0))
                .text_color(theme::muted())
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.text_color(theme::text()).bg(theme::white_a(0x14))))
                .when(!enabled, |d| d.opacity(0.35))
                .child(icon(path, 14.0).text_color(theme::muted()))
        };
        div()
            .absolute()
            .top(px(48.0))
            .right(px(12.0))
            .flex()
            .gap(px(6.0))
            .when(undo_redo, |d| {
                d.child(
                    panel()
                        .flex()
                        .p(px(2.0))
                        .child(small("undo", "icons/undo.svg", self.state.can_undo).on_click(cx.listener(|this, _, _, cx| {
                            this.execute("undo", Exec::default(), cx);
                        })))
                        .child(small("redo", "icons/redo.svg", self.state.can_redo).on_click(cx.listener(|this, _, _, cx| {
                            this.execute("redo", Exec::default(), cx);
                        }))),
                )
            })
            .child(
                tool("save")
                    .text_color(theme::muted())
                    .hover(|s| s.text_color(theme::text()))
                    .child(icon("icons/save.svg", 14.0).text_color(theme::muted()))
                    .child("Save")
                    .on_click(cx.listener(|this, _, _, cx| this.choose_save_location(cx))),
            )
            .child(
                tool("measure")
                    .when(measuring, |d| d.bg(rgba(0xf5c454eb)).border_color(rgba(0x00000000)).text_color(rgba(0x1a1406ff)))
                    .when(!measuring, |d| d.text_color(theme::muted()).hover(|s| s.text_color(theme::text())))
                    .child(icon("icons/measure.svg", 16.0).text_color(if measuring { rgba(0x1a1406ff) } else { theme::muted() }))
                    .child("Measure")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.measuring = if this.measuring.is_some() { None } else { Some(Vec::new()) };
                        cx.notify();
                    })),
            )
    }

    fn render_measure_hint(&self) -> Option<impl IntoElement> {
        let picked = self.measuring.as_ref()?;
        let hint = div().flex().gap(px(6.0)).when_else(
            picked.is_empty(),
            |d| d.child("Click an atom to start a distance"),
            |d| d.child(div().font_family(theme::MONO).child(picked[0].label.clone())).child("→ click a second atom"),
        );
        Some(
            div().absolute().top(px(48.0)).left_0().right_0().flex().justify_center().child(
                panel()
                    .px(px(12.0))
                    .py(px(6.0))
                    .text_size(px(12.0))
                    .flex()
                    .gap(px(6.0))
                    .child(hint)
                    .child(div().text_color(theme::faint()).child("· Esc to finish")),
            ),
        )
    }

    fn render_info(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let objects = &self.state.objects;
        let info = self.info.as_ref();
        let name = match objects.as_slice() {
            [one] => one.name.clone(),
            many => format!("{} objects", many.len()),
        };
        let total = |f: fn(&backend::ObjectSummary) -> usize| objects.iter().map(f).sum::<usize>();
        let row = |label: &'static str, value: String| {
            div().flex().justify_between().child(div().text_color(theme::muted()).child(label)).child(div().child(value))
        };
        let several = objects.len() > 1;
        let first = objects.first().map(|o| o.name.clone()).unwrap_or_default();

        panel()
            .id("info")
            .absolute()
            .top(px(48.0))
            .left(px(12.0))
            .w(px(220.0))
            .max_h(px(f32::MAX))
            .px(px(16.0))
            .py(px(14.0))
            .child(div().mb(px(10.0)).text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).child(name))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(row("Atoms", fmt(total(|o| o.atoms))))
                    .child(row("Residues", fmt(total(|o| o.residues))))
                    .when(!several, |d| d.child(row("States", fmt(info.map_or(1, |i| i.states))))),
            )
            .when(several, |d| {
                d.child(theme::heading("Objects")).child(div().flex().flex_col().gap(px(2.0)).children(objects.iter().enumerate().map(|(i, o)| {
                    let (name, enabled) = (o.name.clone(), o.enabled);
                    let toggle = format!("{} {name}", if enabled { "disable" } else { "enable" });
                    let align = format!("align {name}, {first}");
                    let delete = format!("delete {name}");
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .text_size(px(12.0))
                        .child(
                            div()
                                .id(SharedString::from(format!("eye-{name}")))
                                .p(px(2.0))
                                .cursor_pointer()
                                .text_color(theme::muted())
                                .hover(|s| s.text_color(theme::text()))
                                .child(icon(if enabled { "icons/eye.svg" } else { "icons/eye-off.svg" }, 14.0).text_color(theme::muted()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.execute(&toggle, Exec::default(), cx);
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_family(theme::MONO)
                                .text_color(if enabled { theme::text() } else { theme::faint() })
                                .child(name.clone()),
                        )
                        .when(i > 0, |d| {
                            d.child(chip(format!("align-{name}"), "Align").text_size(px(11.0)).px(px(7.0)).py(px(1.0)).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.execute(&align, Exec::default(), cx);
                                },
                            )))
                        })
                        .child(
                            div()
                                .id(SharedString::from(format!("remove-{name}")))
                                .px(px(2.0))
                                .cursor_pointer()
                                .text_color(theme::muted())
                                .hover(|s| s.text_color(theme::danger()))
                                .child("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.execute(&delete, Exec::default(), cx);
                                })),
                        )
                })))
            })
            .child(theme::heading("Style"))
            .child(div().flex().flex_col().gap(px(2.0)).p(px(3.0)).rounded(px(9.0)).bg(theme::white_a(0x0a)).children(
                PRESETS.iter().enumerate().map(|(i, (name, _))| {
                    let active = self.active_look == i;
                    div()
                        .id(SharedString::from(format!("look-{i}")))
                        .px(px(8.0))
                        .py(px(5.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .text_color(if active { theme::text() } else { theme::muted() })
                        .when(active, |d| d.bg(theme::accent_a(0x29)))
                        .hover(|s| s.text_color(theme::text()))
                        .child(*name)
                        .on_click(cx.listener(move |this, _, _, cx| this.apply_look(i, cx)))
                }),
            ))
            .child(theme::heading("Color"))
            .child(div().flex().flex_wrap().gap(px(4.0)).children(SCHEMES.iter().map(|(label, command, _)| {
                let command = format!("color {command}");
                chip(format!("scheme-{label}"), *label).on_click(cx.listener(move |this, _, _, cx| {
                    this.execute(&command, Exec::default(), cx);
                }))
            })))
            .when(!self.state.measurements.is_empty(), |d| {
                d.child(theme::heading("Measurements")).child(div().flex().flex_col().gap(px(2.0)).children(self.state.measurements.iter().map(|m| {
                    let delete = format!("delete {}", m.name);
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .text_size(px(12.0))
                        .font_family(theme::MONO)
                        .child(div().text_color(theme::muted()).child(m.name.clone()))
                        .child(div().ml_auto().text_color(theme::amber()).child(m.label.clone()))
                        .child(
                            div()
                                .id(SharedString::from(format!("delete-{}", m.name)))
                                .px(px(2.0))
                                .cursor_pointer()
                                .text_color(theme::faint())
                                .hover(|s| s.text_color(theme::danger()))
                                .child("×")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.execute(&delete, Exec::default(), cx);
                                })),
                        )
                })))
            })
            .child(theme::heading("Chains"))
            .children(objects.iter().map(|o| {
                let prefix = if several { format!("{} and ", o.name) } else { String::new() };
                div()
                    .when(several, |d| d.child(div().mt(px(6.0)).mb(px(3.0)).text_size(px(11.0)).font_family(theme::MONO).text_color(theme::faint()).child(o.name.clone())))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.0))
                            .children(o.chains.iter().take(40).map(|chain| {
                                let command = format!("select {prefix}chain {}", if chain.is_empty() { "''" } else { chain });
                                div()
                                    .id(SharedString::from(format!("chain-{}-{chain}", o.name)))
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .rounded(px(6.0))
                                    .bg(theme::white_a(0x0f))
                                    .text_color(theme::muted())
                                    .text_size(px(11.0))
                                    .font_family(theme::MONO)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::white_a(0x1f)).text_color(theme::text()))
                                    .child(if chain.is_empty() { "–".to_owned() } else { chain.clone() })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.execute(&command, Exec::default(), cx);
                                    }))
                            }))
                            .when(o.chains.len() > 40, |d| d.child(div().text_size(px(11.0)).text_color(theme::faint()).child(format!("+{}", o.chains.len() - 40)))),
                    )
            }))
    }

    fn render_start(&self, dropping: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let mod_key = if cfg!(target_os = "macos") { "⌘O" } else { "Ctrl+O" };
        let pill = |id: String, label: String| {
            div()
                .id(SharedString::from(id))
                .px(px(10.0))
                .py(px(4.0))
                .rounded_full()
                .border_1()
                .border_color(theme::border())
                .bg(theme::white_a(0x0d))
                .text_size(px(12.0))
                .cursor_pointer()
                .hover(|s| s.border_color(theme::accent()).bg(theme::accent_a(0x1f)))
                .child(label)
        };
        let content = if self.loading {
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div().size(px(18.0)).rounded_full().border_2().border_color(theme::white_a(0x26)).child(
                        icon("icons/arc.svg", 18.0)
                            .text_color(theme::accent())
                            .absolute()
                            .top(px(-2.0))
                            .left(px(-2.0))
                            .with_animation("spin", Animation::new(Duration::from_millis(800)).repeat(), |svg, t| {
                                svg.with_transformation(Transformation::rotate(percentage(t)))
                            }),
                    ),
                )
                .child("Loading…")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.0))
                .child(div().text_size(px(15.0)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text()).child("Drop a structure file"))
                .child(
                    div().flex().items_center().gap(px(4.0)).text_size(px(12.0)).child(".cif · .pdb · .gz · .rsmol — or type").child(
                        div().px(px(5.0)).rounded(px(4.0)).bg(theme::white_a(0x12)).text_color(theme::text()).font_family(theme::MONO).child("fetch 1hsg"),
                    ),
                )
                .child(
                    div()
                        .id("open")
                        .mt(px(10.0))
                        .px(px(14.0))
                        .py(px(6.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::white_a(0x0f))
                        .text_color(theme::text())
                        .cursor_pointer()
                        .hover(|s| s.border_color(theme::accent()).bg(theme::accent_a(0x1f)))
                        .flex()
                        .gap(px(6.0))
                        .child("Open…")
                        .child(div().text_color(theme::faint()).child(mod_key))
                        .on_click(cx.listener(|this, _, _, cx| this.choose_file(cx))),
                )
                .when(!dropping, |d| {
                    d.child(
                        div()
                            .mt(px(14.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .when(!self.prefs.recent.is_empty(), |d| {
                                d.child(theme::heading("Recent").mt(px(10.0))).child(
                                    div().flex().flex_wrap().justify_center().gap(px(6.0)).max_w(px(360.0)).children(self.prefs.recent.iter().enumerate().map(
                                        |(i, r)| {
                                            let recent = r.clone();
                                            pill(format!("recent-{i}"), r.label().to_owned())
                                                .on_click(cx.listener(move |this, _, _, cx| this.reopen(recent.clone(), cx)))
                                        },
                                    )),
                                )
                            })
                            .child(theme::heading("Try").mt(px(10.0)))
                            .child(div().flex().flex_wrap().justify_center().gap(px(6.0)).children(EXAMPLES.iter().map(|(id, _)| {
                                let command = format!("fetch {id}");
                                pill(format!("example-{id}"), (*id).to_owned()).font_family(theme::MONO).on_click(cx.listener(move |this, _, _, cx| {
                                    this.execute(&command, Exec::default(), cx);
                                }))
                            }))),
                    )
                })
                .into_any_element()
        };
        div().absolute().inset_0().flex().items_center().justify_center().child(
            div()
                .occlude()
                .px(px(56.0))
                .py(px(36.0))
                .rounded(px(18.0))
                .border_2()
                .border_dashed()
                .border_color(if dropping { theme::accent() } else { theme::white_a(0x24) })
                .when(dropping, |d| d.bg(theme::accent_a(0x14)))
                .text_color(theme::muted())
                .child(content),
        )
    }

    fn render_toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let error = self.error.clone()?;
        Some(
            div().absolute().top(px(48.0)).left_0().right_0().flex().justify_center().child(
                panel()
                    .max_w(relative_width(0.6))
                    .px(px(14.0))
                    .py(px(10.0))
                    .flex()
                    .gap(px(12.0))
                    .text_color(theme::danger())
                    .child(div().flex_1().child(error))
                    .child(
                        div()
                            .id("dismiss")
                            .cursor_pointer()
                            .text_color(theme::muted())
                            .text_size(px(16.0))
                            .child("×")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.error = None;
                                cx.notify();
                            })),
                    ),
            ),
        )
    }

    fn render_selection_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sel = self.state.selection.as_ref()?;
        let button = |id: String, label: &'static str| {
            div()
                .id(SharedString::from(id))
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(theme::border())
                .bg(theme::white_a(0x0d))
                .cursor_pointer()
                .hover(|s| s.bg(theme::white_a(0x1a)).border_color(theme::white_a(0x29)))
                .child(label)
        };
        Some(
            panel()
                .px(px(12.0))
                .py(px(10.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(div().size(px(8.0)).rounded_full().bg(rgba(0xff5cc8ff)).shadow(vec![wgpui::BoxShadow {
                            color: wgpui::hsla(0.88, 1.0, 0.68, 0.6),
                            offset: Point::default(),
                            blur_radius: px(8.0),
                            spread_radius: px(0.0),
                            inset: false,
                        }]))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("sele"))
                        .child(div().text_color(theme::muted()).child(format!("{} atoms · {} residues", fmt(sel.atoms), fmt(sel.residues))))
                        .child(
                            div()
                                .id("deselect")
                                .ml_auto()
                                .px(px(4.0))
                                .py(px(2.0))
                                .cursor_pointer()
                                .text_color(theme::muted())
                                .hover(|s| s.text_color(theme::text()))
                                .child("Clear")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.execute("deselect", Exec::default(), cx);
                                })),
                        ),
                )
                .child(div().flex().flex_wrap().gap(px(6.0)).children(SELECTION_ACTIONS.iter().map(|(label, command)| {
                    button(format!("sele-{label}"), label).on_click(cx.listener(move |this, _, _, cx| {
                        this.execute(command, Exec::default(), cx);
                    }))
                })))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(6.0))
                        .children(SWATCHES.iter().map(|(name, hex)| {
                            let command = format!("color {name}, sele");
                            div()
                                .id(SharedString::from(format!("swatch-{name}")))
                                .size(px(20.0))
                                .rounded_full()
                                .border_2()
                                .border_color(theme::white_a(0x1a))
                                .bg(wgpui::rgb(*hex))
                                .cursor_pointer()
                                .hover(|s| s.border_color(theme::white_a(0x80)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.execute(&command, Exec::default(), cx);
                                }))
                        }))
                        .child(button("sele-elements".into(), "Elements").ml(px(4.0)).text_size(px(12.0)).py(px(3.0)).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.execute("color byelement, sele", Exec::default(), cx);
                            },
                        ))),
                )
                .into_any_element(),
        )
    }

    fn render_command_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = match &self.result {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        };
        let carets = error.as_ref().map(|e| {
            let input = self.input.read(cx);
            let x = input.x_for_char(e.start);
            let width = (input.x_for_char(e.end.max(e.start + 1)) - x).max(px(7.0));
            div().ml(x).w(width).h(px(2.0)).mt(px(1.0)).bg(theme::danger()).rounded_full()
        });
        let status = if let Some(line) = &self.running {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().flex_none().size(px(7.0)).rounded_full().bg(theme::accent()).with_animation(
                        "pulse",
                        Animation::new(Duration::from_millis(1000)).repeat(),
                        |dot, t| dot.opacity(1.0 - 0.7 * (t * std::f32::consts::PI).sin()),
                    ))
                    .child(format!("running Python… {line}"))
                    .into_any_element(),
            )
        } else {
            self.result.as_ref().map(|r| match r {
                Ok(r) => div()
                    .child(match (&r.canonical, r.atoms) {
                        (Some(canonical), Some(atoms)) => format!("{} atoms · {canonical}", fmt(atoms)),
                        _ => r.message.clone(),
                    })
                    .into_any_element(),
                Err(e) => div().text_color(theme::danger()).child(e.message.clone()).into_any_element(),
            })
        };
        let expanded = self.prefs.history_expanded;
        let _ = window;
        panel()
            .px(px(14.0))
            .py(px(10.0))
            .font_family(theme::MONO)
            .when(expanded && !self.history.is_empty(), |d| {
                d.child(
                    div()
                        .id("history")
                        .max_h(px(132.0))
                        .overflow_y_scroll()
                        .track_scroll(&self.history_scroll)
                        .mb(px(8.0))
                        .pb(px(8.0))
                        .border_b_1()
                        .border_color(theme::border())
                        .text_size(px(12.0))
                        .text_color(theme::muted())
                        .children(self.history.iter().map(|entry| {
                            div()
                                .flex()
                                .flex_col()
                                .py(px(2.0))
                                .child(div().text_color(theme::text()).child(format!("› {}", entry.input)))
                                .child(div().when(!entry.ok, |d| d.text_color(theme::danger())).child(entry.output.clone()))
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.0))
                    .text_size(px(13.0))
                    .child(div().text_color(theme::accent()).child("rsmol›"))
                    .child(div().flex_1().flex().flex_col().child(self.input.clone()).children(carets))
                    .when(!self.history.is_empty(), |d| {
                        d.child(
                            div()
                                .id("history-toggle")
                                .flex()
                                .items_center()
                                .gap(px(4.0))
                                .px(px(7.0))
                                .py(px(2.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::white_a(0x0a))
                                .text_color(theme::muted())
                                .text_size(px(11.0))
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text()).bg(theme::white_a(0x17)))
                                // Points up (expand) when collapsed and down (collapse) when open.
                                .child(icon(if expanded { "icons/chevron-down.svg" } else { "icons/chevron-up.svg" }, 10.0).text_color(theme::muted()))
                                .child(self.history.len().to_string())
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_history(cx))),
                        )
                    }),
            )
            .children(status.map(|s| div().mt(px(8.0)).text_size(px(12.0)).text_color(theme::muted()).child(s)))
    }
}

fn relative_width(fraction: f32) -> wgpui::Length {
    wgpui::relative(fraction).into()
}

impl Render for RsmolApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.scale = window.scale_factor();
        if self.warmup > 0 {
            self.warmup -= 1;
            window.request_animation_frame();
        }
        let dropping = cx.has_active_drag();
        let show_start = self.info.is_none() || dropping || self.loading;

        div()
            .id("root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgba(0x040508ff))
            .font_family(theme::UI)
            .text_size(px(13.0))
            .line_height(px(18.0))
            .text_color(theme::text())
            .track_focus(&self.focus)
            .key_context("Workspace")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &OpenFile, _, cx| this.choose_file(cx)))
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| this.choose_save_location(cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open_file(path.clone(), cx);
                }
            }))
            .child(self.render_viewport(cx))
            .child(self.render_labels(window))
            .child(self.render_titlebar(window))
            .when(self.info.is_some(), |d| d.child(self.render_tools(cx)).children(self.render_measure_hint()).child(self.render_info(cx)))
            .when(show_start, |d| d.child(self.render_start(dropping, cx)))
            .children(self.render_toast(cx))
            .child(
                div()
                    .absolute()
                    .left(px(12.0))
                    .right(px(12.0))
                    .bottom(px(12.0))
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .flex_1()
                            .max_w(px(760.0))
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .children(self.render_selection_card(cx))
                            .child(self.render_command_bar(window, cx)),
                    ),
            )
            .children(self.stats.as_ref().map(|(ms, gpu)| {
                panel()
                    .absolute()
                    .right(px(12.0))
                    .bottom(px(12.0))
                    .px(px(10.0))
                    .py(px(6.0))
                    .text_size(px(11.0))
                    .font_family(theme::MONO)
                    .text_color(theme::muted())
                    .child(format!("{ms:.2} ms · {gpu}"))
            }))
            .children(self.render_tooltip())
    }
}

/// File types the open dialog and drops accept, for messages.
pub fn is_openable(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| STRUCTURE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Opens `path` once the window exists (from the command line).
pub fn open_at_start(app: &Entity<RsmolApp>, path: PathBuf, cx: &mut App) {
    app.update(cx, |app, cx| app.open_file(path, cx));
}

/// Registers the app-wide key bindings; ⌘ on macOS, Ctrl elsewhere.
pub fn bind_keys(cx: &mut App) {
    let m = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
    cx.bind_keys([
        wgpui::KeyBinding::new(&format!("{m}-o"), OpenFile, None),
        wgpui::KeyBinding::new(&format!("{m}-s"), SaveFile, None),
        wgpui::KeyBinding::new(&format!("{m}-q"), Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}
