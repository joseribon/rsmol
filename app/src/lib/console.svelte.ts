// Shared command execution and loading: the command bar, preset buttons, selection card and
// file drops all go through here, so every action lands in one history showing the PyMOL syntax
// behind it.
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  fetchStructure,
  loadStructure,
  runCommand,
  runPython,
  saveSession,
  sessionState,
  type CommandResult,
  type SelectionError,
  type MeasurementSummary,
  type ObjectSummary,
  type PickInfo,
  type SelectionSummary,
  type StructureInfo,
} from "./api";

export type Outcome = { ok: true; value: CommandResult } | { ok: false; error: SelectionError };
export interface Entry {
  input: string;
  output: string;
  ok: boolean;
}
/** Something that can be reopened from the start screen. */
export type Recent = { kind: "file"; path: string; name: string } | { kind: "fetch"; id: string };

export const consoleState = $state({
  history: [] as Entry[],
  /** The current "sele", or null when nothing is selected. */
  selection: null as SelectionSummary | null,
  structure: null as StructureInfo | null,
  loading: false,
  error: null as string | null,
  recent: readRecent(),
  /** The atom under the pointer and where to show its tooltip (CSS pixels). */
  hover: null as { info: PickInfo; x: number; y: number } | null,
  measurements: [] as MeasurementSummary[],
  canUndo: false,
  canRedo: false,
  /** Loaded objects, in scene order; empty shows the start screen. */
  objects: [] as ObjectSummary[],
  /** Measure tool: atoms picked so far, or null when the tool is off. */
  measuring: null as PickInfo[] | null,
  /** While a Python script runs: its latest output line ("" until it prints). */
  running: null as string | null,
});

/** Called by the app for each line a running script prints. */
export function scriptOutput(line: string) {
  if (consoleState.running !== null) consoleState.running = line;
}

/** Re-reads session state, e.g. after a running script changed it. */
export async function refreshState() {
  applyState(await sessionState());
}

/** Turns the measure tool on or off. */
export function toggleMeasuring() {
  consoleState.measuring = consoleState.measuring ? null : [];
}

/** Adds a picked atom; every second atom completes a distance. */
export async function measurePick(info: PickInfo) {
  const picked = [...(consoleState.measuring ?? []), info];
  if (picked.length < 2) {
    consoleState.measuring = picked;
    return;
  }
  consoleState.measuring = [];
  await execute(`distance index ${picked[0].index}, index ${picked[1].index}`);
}

const HISTORY_LIMIT = 5;
const RECENT_KEY = "rsmol.recent";
const RECENT_LIMIT = 6;
const FETCH = /^\s*fetch\s+(\S+)\s*$/i;
const RUN = /^\s*run\s+(.+?)\s*$/i;
const PYTHON = /^\s*python\s+(.+)$/is;
/** Lines of script output kept in the history entry. */
const OUTPUT_LINES = 30;

/** Runs `input`. Real runs are logged; failures too when `logErrors` (button clicks). */
export async function execute(input: string, { dryRun = false, logErrors = true } = {}): Promise<Outcome | null> {
  if (!input.trim()) return null;
  const fetchMatch = input.match(FETCH);
  const runMatch = input.match(RUN);
  const pythonMatch = input.match(PYTHON);
  let outcome: Outcome;
  if (runMatch || pythonMatch) {
    // Scripts run in the app's Python, outside the core session.
    const script = runMatch ? { path: runMatch[1] } : { code: pythonMatch![1] };
    const what = runMatch ? `run ${runMatch[1]} with Python` : "run Python code";
    outcome = dryRun ? { ok: true, value: { ...emptyResult(), message: what } } : await runScript(script);
  } else if (fetchMatch) {
    // Downloading happens in the app, not the core session, so `fetch` is handled here.
    const id = fetchMatch[1].toUpperCase();
    outcome = dryRun
      ? { ok: true, value: { ...emptyResult(), message: `fetch ${id} from the RCSB PDB` } }
      : await fetchEntry(id);
  } else {
    try {
      outcome = { ok: true, value: await runCommand(input, dryRun) };
    } catch (error) {
      outcome = { ok: false, error: error as SelectionError };
    }
  }
  if (dryRun) return outcome;
  if (outcome.ok) {
    applyState(outcome.value);
    log({ input, output: outcome.value.message, ok: true });
  } else if (logErrors) {
    log({ input, output: outcome.error.message, ok: false });
  }
  return outcome;
}

const STRUCTURE_EXTENSIONS = ["cif", "mmcif", "pdb", "ent", "gz"];
const SESSION_EXTENSION = "rsmol";

async function runScript(script: { path?: string; code?: string }): Promise<Outcome> {
  consoleState.running = "";
  try {
    const { code, output } = await runPython(script);
    const lines = output.trimEnd().split("\n");
    const tail = (lines.length > OUTPUT_LINES ? ["…", ...lines.slice(-OUTPUT_LINES)] : lines).join("\n");
    if (code === 0) return { ok: true, value: { ...emptyResult(), message: tail || "done" } };
    return { ok: false, error: { start: 0, end: 0, message: tail || `Python exited with code ${code}` } };
  } catch (e) {
    return { ok: false, error: { start: 0, end: 0, message: String(e) } };
  } finally {
    consoleState.running = null;
    await refreshState();
  }
}

/** Loads a structure or `.rsmol` session (dropped, chosen, passed on the command line, recent),
 * or runs a `.py` script. */
export async function openFile(path: string) {
  if (path.toLowerCase().endsWith(".py")) {
    await execute(`run ${path}`);
    return;
  }
  const info = await loading(() => loadStructure(path));
  if (!info) return;
  const isSession = path.toLowerCase().endsWith(`.${SESSION_EXTENSION}`);
  applyState(await sessionState());
  const name = isSession ? (path.split("/").pop() ?? info.name) : info.name;
  remember({ kind: "file", path, name });
  log({ input: isSession ? `open ${name}` : `load ${name}`, output: `${info.atoms.toLocaleString("en-US")} atoms`, ok: true });
}

/** Asks for a structure or session file and opens it. */
export async function chooseFile() {
  const path = await openDialog({
    multiple: false,
    directory: false,
    filters: [
      { name: "Structures and sessions", extensions: [...STRUCTURE_EXTENSIONS, SESSION_EXTENSION] },
      { name: "rsmol session", extensions: [SESSION_EXTENSION] },
      { name: "Python script", extensions: ["py"] },
    ],
  });
  if (typeof path === "string") await openFile(path);
}

/** Asks where to save, then writes the session there. */
export async function chooseSaveLocation() {
  if (!consoleState.structure) return;
  const path = await saveDialog({
    defaultPath: `${consoleState.objects[0]?.name ?? consoleState.structure.name}.${SESSION_EXTENSION}`,
    filters: [
      { name: "rsmol session", extensions: [SESSION_EXTENSION] },
      { name: "PDB", extensions: ["pdb"] },
      { name: "mmCIF", extensions: ["cif"] },
    ],
  });
  if (!path) return;
  if (path.toLowerCase().endsWith(`.${SESSION_EXTENSION}`)) {
    try {
      const message = await saveSession(path);
      log({ input: "save session", output: message, ok: true });
      remember({ kind: "file", path, name: path.split("/").pop() ?? path });
    } catch (e) {
      log({ input: "save session", output: String(e), ok: false });
    }
  } else {
    // Structure formats go through the command, so the history shows how to script it.
    await execute(`save ${path}`);
  }
}

async function fetchEntry(id: string): Promise<Outcome> {
  const info = await loading(() => fetchStructure(id));
  if (!info) {
    const message = consoleState.error ?? "fetch failed";
    consoleState.error = null;
    return { ok: false, error: { start: 0, end: 0, message } };
  }
  remember({ kind: "fetch", id });
  applyState(await sessionState());
  const message = `${info.name}: ${info.atoms.toLocaleString("en-US")} atoms in ${info.parse_ms.toFixed(0)} ms`;
  return { ok: true, value: { ...emptyResult(), message, atoms: info.atoms } };
}

export function reopen(r: Recent) {
  return r.kind === "file" ? openFile(r.path) : execute(`fetch ${r.id}`);
}

export const recentLabel = (r: Recent) => (r.kind === "file" ? r.name : r.id);

async function loading(load: () => Promise<StructureInfo>): Promise<StructureInfo | null> {
  consoleState.loading = true;
  consoleState.error = null;
  try {
    const info = await load();
    consoleState.structure = info;
    if (consoleState.measuring) consoleState.measuring = [];
    return info;
  } catch (e) {
    consoleState.error = String(e);
    return null;
  } finally {
    consoleState.loading = false;
  }
}

function applyState(r: CommandResult) {
  consoleState.selection = r.selection;
  consoleState.measurements = r.measurements;
  consoleState.canUndo = r.can_undo;
  consoleState.canRedo = r.can_redo;
  consoleState.objects = r.objects;
  // Everything deleted: back to the start screen.
  if (r.objects.length === 0) consoleState.structure = null;
  // A script loaded the first structure: leave the start screen.
  else if (!consoleState.structure) {
    const o = r.objects[0];
    consoleState.structure = { name: o.name, path: "", atoms: o.atoms, residues: o.residues, chains: o.chains, states: 1, parse_ms: 0 };
  }
}

/** A result for actions handled in the UI (fetch), with no session state attached. */
function emptyResult(): CommandResult {
  return {
    message: "",
    canonical: null,
    atoms: null,
    selection: consoleState.selection,
    measurements: consoleState.measurements,
    can_undo: consoleState.canUndo,
    can_redo: consoleState.canRedo,
    objects: consoleState.objects,
  };
}

function log(entry: Entry) {
  consoleState.history = [...consoleState.history.slice(-(HISTORY_LIMIT - 1)), entry];
}

function remember(r: Recent) {
  const same = (a: Recent) => recentLabel(a) === recentLabel(r) && a.kind === r.kind;
  consoleState.recent = [r, ...consoleState.recent.filter((x) => !same(x))].slice(0, RECENT_LIMIT);
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(consoleState.recent));
  } catch {
    // Storage unavailable: recents only last for this session.
  }
}

function readRecent(): Recent[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}
