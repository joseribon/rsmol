// Shared command execution and loading: the command bar, preset buttons, selection card and
// file drops all go through here, so every action lands in one history showing the PyMOL syntax
// behind it.
import {
  fetchStructure,
  loadStructure,
  runCommand,
  type CommandResult,
  type SelectionError,
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
});

const HISTORY_LIMIT = 5;
const RECENT_KEY = "rsmol.recent";
const RECENT_LIMIT = 6;
const FETCH = /^\s*fetch\s+(\S+)\s*$/i;

/** Runs `input`. Real runs are logged; failures too when `logErrors` (button clicks). */
export async function execute(input: string, { dryRun = false, logErrors = true } = {}): Promise<Outcome | null> {
  if (!input.trim()) return null;
  const fetchMatch = input.match(FETCH);
  let outcome: Outcome;
  if (fetchMatch) {
    // Downloading happens in the app, not the core session, so `fetch` is handled here.
    const id = fetchMatch[1].toUpperCase();
    outcome = dryRun
      ? { ok: true, value: { message: `fetch ${id} from the RCSB PDB`, canonical: null, atoms: null, selection: null } }
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
    consoleState.selection = outcome.value.selection;
    log({ input, output: outcome.value.message, ok: true });
  } else if (logErrors) {
    log({ input, output: outcome.error.message, ok: false });
  }
  return outcome;
}

/** Loads a local file (dropped, passed on the command line, or recent). */
export async function openFile(path: string) {
  const info = await loading(() => loadStructure(path));
  if (info) remember({ kind: "file", path, name: info.name });
}

async function fetchEntry(id: string): Promise<Outcome> {
  const info = await loading(() => fetchStructure(id));
  if (!info) {
    const message = consoleState.error ?? "fetch failed";
    consoleState.error = null;
    return { ok: false, error: { start: 0, end: 0, message } };
  }
  remember({ kind: "fetch", id });
  const message = `${info.name}: ${info.atoms.toLocaleString("en-US")} atoms in ${info.parse_ms.toFixed(0)} ms`;
  return { ok: true, value: { message, canonical: null, atoms: info.atoms, selection: null } };
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
    consoleState.selection = null;
    return info;
  } catch (e) {
    consoleState.error = String(e);
    return null;
  } finally {
    consoleState.loading = false;
  }
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
