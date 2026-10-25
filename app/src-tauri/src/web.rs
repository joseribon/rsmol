//! Viewport drawn by the webview itself (Linux, Windows), where a native surface cannot sit
//! under the web UI. The frontend runs `WebRenderer` (crates/rsmol-web) in a canvas; this side
//! streams it the scene and view requests over one ordered channel, so a display update can
//! never overtake the scene it belongs to.
//!
//! Every message is binary: a tag byte, then
//! - `SOURCE`: `[key u32][name length u32][file name][file bytes]`, a structure file to parse;
//! - `DISPLAY`: per-atom representation flags, then RGBA per atom;
//! - `JSON`: a small UTF-8 JSON request (scene, highlight, focus, camera, measurements).
//!
//! When the page (re)attaches, the whole session is replayed, so nothing sent before the UI was
//! ready is lost and a webview reload recovers.

use std::sync::{Arc, Mutex};

use rsmol_core::Structure;
use rsmol_core::display::Display;
use rsmol_core::measure::Measurement;
use rsmol_core::session::Session;
use rsmol_core::session_file::CameraState;
use serde_json::{Value, json};
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::viewport::{CameraAction, Msg};

const JSON: u8 = 0;
const SOURCE: u8 = 1;
const DISPLAY: u8 = 2;

#[derive(Default)]
pub struct WebViewport(Mutex<State>);

#[derive(Default)]
struct State {
    channel: Option<Channel<InvokeResponseBody>>,
    /// Files the page has parsed, by key; identity is the file data's allocation.
    sent: Vec<(Arc<Vec<u8>>, u32)>,
    next_key: u32,
    /// Files written for objects that have none (kept so they are not re-exported each time).
    exported: Vec<(Arc<Structure>, Arc<Vec<u8>>)>,
    /// The scene the page shows, for measurement geometry.
    structure: Option<Arc<Structure>>,
    /// The page's camera, as it last reported it, for saving sessions.
    camera: Option<CameraState>,
}

impl State {
    fn post(&self, tag: u8, body: &[u8]) {
        if let Some(channel) = &self.channel {
            let mut message = Vec::with_capacity(body.len() + 1);
            message.push(tag);
            message.extend_from_slice(body);
            let _ = channel.send(InvokeResponseBody::Raw(message));
        }
    }

    fn json(&self, value: Value) {
        self.post(JSON, value.to_string().as_bytes());
    }

    fn display(&self, display: &Display) {
        let mut body: Vec<u8> = display.reps.iter().map(|r| r.bits()).collect();
        body.extend(display.colors.iter().flatten());
        self.post(DISPLAY, &body);
    }

    fn measurements(&self, measurements: &[Measurement]) {
        let coords = self.structure.as_ref().and_then(|s| s.coords(0)).unwrap_or_default();
        let items: Vec<Value> = measurements
            .iter()
            .map(|m| {
                let segments: Vec<f32> = m.segments(coords).into_iter().flat_map(|(a, b)| a.into_iter().chain(b)).collect();
                json!({ "name": m.name, "text": m.label(), "anchor": m.anchor(coords), "segments": segments })
            })
            .collect();
        self.json(json!({ "type": "measurements", "items": items }));
    }
}

impl WebViewport {
    /// Connects the page's channel and replays the session onto it.
    pub fn attach(&self, channel: Channel<InvokeResponseBody>, session: &Session) {
        let camera = {
            let mut state = self.0.lock().unwrap();
            state.channel = Some(channel);
            state.sent.clear();
            state.camera
        };
        self.scene(session, true);
        let state = self.0.lock().unwrap();
        state.measurements(&session.measurements);
        let highlight: Vec<usize> = session.named.get("sele").map(|set| set.iter().collect()).unwrap_or_default();
        state.json(json!({ "type": "highlight", "atoms": highlight }));
        if let Some(camera) = camera {
            state.json(json!({ "type": "set_camera", "state": camera }));
        }
    }

    /// Sends the session's objects (files the page has not parsed yet, then the object list)
    /// and their display.
    pub fn scene(&self, session: &Session, refit: bool) {
        let mut guard = self.0.lock().unwrap();
        let state = &mut *guard;
        let mut keys = Vec::with_capacity(session.objects.len());
        let mut transforms: Vec<f32> = Vec::with_capacity(session.objects.len() * 12);
        let mut sent = Vec::with_capacity(session.objects.len());
        for object in &session.objects {
            let (name, data) = match &object.source {
                Some(source) => (source.file_name.clone(), source.data.clone()),
                None => {
                    // Coordinates already include the transform, which is then identity-like
                    // for the page: it gets exactly these coordinates.
                    let data = match state.exported.iter().find(|(s, _)| Arc::ptr_eq(s, &object.structure)) {
                        Some((_, data)) => data.clone(),
                        None => {
                            let all: Vec<usize> = (0..object.structure.atom_count()).collect();
                            let data = Arc::new(rsmol_core::io::write::to_mmcif(&object.structure, &all).into_bytes());
                            state.exported.push((object.structure.clone(), data.clone()));
                            data
                        }
                    };
                    (format!("{}.cif", object.name), data)
                }
            };
            let key = match state.sent.iter().find(|(d, _)| Arc::ptr_eq(d, &data)) {
                Some(&(_, key)) => key,
                None => {
                    let key = state.next_key;
                    state.next_key += 1;
                    let mut body = Vec::with_capacity(8 + name.len() + data.len());
                    body.extend(key.to_le_bytes());
                    body.extend((name.len() as u32).to_le_bytes());
                    body.extend(name.as_bytes());
                    body.extend_from_slice(&data);
                    state.post(SOURCE, &body);
                    key
                }
            };
            let transform = if object.source.is_some() { object.transform } else { rsmol_core::superpose::Transform::IDENTITY };
            transforms.extend(transform.rotation.iter().flatten().chain(&transform.translation));
            keys.push(key);
            sent.push((data, key));
        }
        // The page forgets files that left the scene; so do we.
        state.sent = sent;
        state.exported.retain(|(s, _)| session.objects.iter().any(|o| Arc::ptr_eq(s, &o.structure)));
        state.structure = session.structure.clone();
        state.json(json!({ "type": "scene", "keys": keys, "transforms": transforms, "refit": refit }));
        if let Some(display) = session.render_display() {
            state.display(&display);
        }
    }

    /// Records the page's camera, for saving sessions.
    pub fn set_camera(&self, camera: CameraState) {
        self.0.lock().unwrap().camera = Some(camera);
    }

    /// The native viewport's messages, translated for the page. Scene changes go through
    /// `scene`, which needs the whole session.
    pub fn send(&self, msg: Msg) {
        let mut state = self.0.lock().unwrap();
        match msg {
            Msg::Camera(action) => {
                let action = match action {
                    CameraAction::Orbit { dx, dy } => json!({ "kind": "orbit", "dx": dx, "dy": dy }),
                    CameraAction::Pan { dx, dy, height } => json!({ "kind": "pan", "dx": dx, "dy": dy, "height": height }),
                    CameraAction::Zoom { factor } => json!({ "kind": "zoom", "factor": factor }),
                    CameraAction::Turn { axis, degrees } => json!({ "kind": "turn", "axis": axis, "degrees": degrees }),
                    CameraAction::Reset => json!({ "kind": "reset" }),
                };
                state.json(json!({ "type": "camera", "action": action }));
            }
            Msg::Display(display) => state.display(&display),
            Msg::Highlight(atoms) => state.json(json!({ "type": "highlight", "atoms": atoms })),
            Msg::Focus(atoms) => state.json(json!({ "type": "focus", "atoms": atoms })),
            Msg::Measurements(measurements) => state.measurements(&measurements),
            // Dropping the reply makes the caller save without a camera at once, rather than
            // time out, if the page never reported one.
            Msg::GetCamera(reply) => {
                if let Some(camera) = state.camera {
                    let _ = reply.send(camera);
                }
            }
            Msg::SetCamera(camera) => {
                state.camera = Some(camera);
                state.json(json!({ "type": "set_camera", "state": camera }));
            }
            // The page sizes its own canvas and picks locally.
            Msg::Resize { .. } => {}
            Msg::Pick { reply, .. } => {
                let _ = reply.send(None);
            }
            // `apply_effects` sends scene changes through `scene`, which has the objects.
            Msg::Scene { .. } => eprintln!("viewport: scene message without a session ignored"),
        }
    }
}
