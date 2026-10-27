// The webview viewport (Linux, Windows): rsmol-web's WebRenderer draws into a canvas, fed by
// the backend over one ordered channel (src-tauri/src/web.rs). Camera, drawing and picking
// are local, so interaction never waits on IPC; frames are drawn on demand, at most one per
// animation frame.
import { Channel, invoke } from "@tauri-apps/api/core";
import type { CameraState, FrameStats, Label } from "./api";

// The parts of the generated bindings (crates/rsmol-web) used here. Declared rather than
// imported: `bun run wasm` generates them on Linux and Windows only, and macOS never needs them.
interface WebRenderer {
  gpu(): string;
  resize(width: number, height: number): void;
  add_source(message: Uint8Array): void;
  set_scene(keys: Uint32Array, transforms: Float32Array, refit: boolean): void;
  set_display(bytes: Uint8Array): void;
  set_highlight(atoms: Uint32Array): void;
  set_measurements(segments: Float32Array): void;
  orbit(dx: number, dy: number): void;
  pan(dx: number, dy: number, height: number): void;
  zoom(factor: number): void;
  turn(axis: string, degrees: number): void;
  reset(): void;
  focus(atoms: Uint32Array): void;
  camera_state(): Float32Array;
  set_camera_state(state: Float32Array): void;
  project(points: Float32Array): Float32Array;
  render(): number;
  pick(x: number, y: number): Promise<number>;
}
interface WasmModule {
  default(): Promise<unknown>;
  WebRenderer: { create(canvas: HTMLCanvasElement, webgl: boolean): Promise<WebRenderer> };
}

const wasmModules = import.meta.glob<WasmModule>("./wasm/rsmol_web.js");

/** What the UI shows from the web renderer, in the same shapes as the native viewport. */
export const webView = $state({
  active: false,
  labels: [] as Label[],
  stats: null as FrameStats | null,
});

interface Measurement {
  name: string;
  text: string;
  anchor: [number, number, number];
  segments: number[];
}

type Request =
  | { type: "scene"; keys: number[]; transforms: number[]; refit: boolean }
  | { type: "highlight"; atoms: number[] }
  | { type: "focus"; atoms: number[] | null }
  | { type: "measurements"; items: Measurement[] }
  | { type: "camera"; action: CameraAction }
  | { type: "set_camera"; state: CameraState };

export type CameraAction =
  | { kind: "orbit"; dx: number; dy: number }
  | { kind: "pan"; dx: number; dy: number; height: number }
  | { kind: "zoom"; factor: number }
  | { kind: "turn"; axis: string; degrees: number }
  | { kind: "reset" };

const JSON_MESSAGE = 0;
const SOURCE = 1;
const DISPLAY = 2;

let renderer: WebRenderer | null = null;
let canvas: HTMLCanvasElement;
let measurements: Measurement[] = [];
let scheduled = false;
let lastStats = 0;
let reportedCamera = "";
let cameraTimer: ReturnType<typeof setTimeout> | undefined;
const decoder = new TextDecoder();

/** Starts drawing into `target`; resolves once the renderer is up and attached. */
export async function startWebViewport(target: HTMLCanvasElement, onError: (message: string) => void) {
  canvas = target;
  const load = wasmModules["./wasm/rsmol_web.js"];
  if (!load) throw new Error("the WebAssembly renderer is missing; run `bun run wasm`");
  const wasm = await load();
  await wasm.default();
  fitCanvas();
  // ?webgl (or localStorage rsmol.webgl=1) skips WebGPU, for testing the fallback.
  const webgl = location.search.includes("webgl") || safeStorage("rsmol.webgl") === "1";
  const created = await wasm.WebRenderer.create(canvas, webgl);
  renderer = created;
  webView.active = true;
  webView.stats = { frame_ms: 0, gpu: created.gpu() };
  new ResizeObserver(() => {
    fitCanvas();
    renderer?.resize(canvas.width, canvas.height);
    requestRender();
  }).observe(canvas);

  const channel = new Channel<ArrayBuffer | number[]>();
  channel.onmessage = (message) => {
    try {
      receive(message instanceof ArrayBuffer ? new Uint8Array(message) : new Uint8Array(message));
    } catch (e) {
      onError(`viewport: ${e}`);
    }
  };
  await invoke("viewport_attach", { channel, gpu: created.gpu() });
  requestRender();
}

function safeStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function fitCanvas() {
  canvas.width = Math.max(1, Math.round(canvas.clientWidth * devicePixelRatio));
  canvas.height = Math.max(1, Math.round(canvas.clientHeight * devicePixelRatio));
}

function receive(bytes: Uint8Array) {
  const r = renderer!;
  const body = bytes.subarray(1);
  switch (bytes[0]) {
    case SOURCE:
      r.add_source(body);
      return;
    case DISPLAY:
      r.set_display(body);
      break;
    case JSON_MESSAGE:
      apply(r, JSON.parse(decoder.decode(body)) as Request);
      break;
  }
  requestRender();
}

function apply(r: WebRenderer, req: Request) {
  switch (req.type) {
    case "scene":
      r.set_scene(new Uint32Array(req.keys), new Float32Array(req.transforms), req.refit);
      if (!req.keys.length) setMeasurements(r, []);
      if (req.refit) reportCamera();
      break;
    case "highlight":
      r.set_highlight(new Uint32Array(req.atoms));
      break;
    case "focus":
      r.focus(new Uint32Array(req.atoms ?? []));
      reportCamera();
      break;
    case "measurements":
      setMeasurements(r, req.items);
      break;
    case "camera":
      camera(req.action);
      break;
    case "set_camera": {
      const s = req.state;
      r.set_camera_state(new Float32Array([...s.target, s.distance, ...s.rotation, s.fov_y]));
      reportCamera();
      break;
    }
  }
}

function setMeasurements(r: WebRenderer, items: Measurement[]) {
  measurements = items;
  r.set_measurements(new Float32Array(items.flatMap((m) => m.segments)));
  if (!items.length) webView.labels = [];
}

/** Moves the camera; deltas and heights in CSS pixels, as the native viewport takes them. */
export function camera(action: CameraAction) {
  const r = renderer;
  if (!r) return;
  switch (action.kind) {
    case "orbit":
      r.orbit(action.dx, action.dy);
      break;
    case "pan":
      r.pan(action.dx, action.dy, action.height);
      break;
    case "zoom":
      r.zoom(action.factor);
      break;
    case "turn":
      r.turn(action.axis, action.degrees);
      break;
    case "reset":
      r.reset();
      break;
  }
  requestRender();
  reportCamera();
}

/** The atom (0-based) under a point in CSS pixels, or null. */
export async function pickAtom(x: number, y: number): Promise<number | null> {
  if (!renderer) return null;
  const atom: number = await renderer.pick(x * devicePixelRatio, y * devicePixelRatio);
  return atom >= 0 ? atom : null;
}

export function requestRender() {
  if (!scheduled && renderer) {
    scheduled = true;
    requestAnimationFrame(frame);
  }
}

function frame() {
  scheduled = false;
  const r = renderer!;
  const ms = r.render();
  if (measurements.length) {
    const xy = r.project(new Float32Array(measurements.flatMap((m) => m.anchor)));
    webView.labels = measurements.map((m, i) => ({
      name: m.name,
      text: m.text,
      position: Number.isNaN(xy[2 * i]) ? null : [xy[2 * i], xy[2 * i + 1]],
    }));
  }
  const now = performance.now();
  if (now - lastStats > 250) {
    lastStats = now;
    webView.stats = { frame_ms: ms, gpu: r.gpu() };
  }
}

/** Tells the backend where the camera is (for saving sessions), at most every 300 ms. */
function reportCamera() {
  if (cameraTimer !== undefined) return;
  cameraTimer = setTimeout(() => {
    cameraTimer = undefined;
    if (!renderer) return;
    const s = Array.from(renderer.camera_state());
    const key = s.join(",");
    if (key === reportedCamera) return;
    reportedCamera = key;
    const state: CameraState = { target: [s[0], s[1], s[2]], distance: s[3], rotation: [s[4], s[5], s[6], s[7]], fov_y: s[8] };
    invoke("viewport_camera", { state }).catch(() => {});
  }, 300);
}
