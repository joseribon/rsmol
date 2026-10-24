// Typed wrappers around the Rust commands in src-tauri/src/main.rs.
import { invoke } from "@tauri-apps/api/core";

export interface StructureInfo {
  name: string;
  path: string;
  atoms: number;
  residues: number;
  chains: string[];
  states: number;
  parse_ms: number;
}

export interface SelectionError {
  start: number;
  end: number;
  message: string;
}

export type CameraAction =
  | { kind: "orbit"; dx: number; dy: number }
  | { kind: "pan"; dx: number; dy: number; height: number }
  | { kind: "zoom"; factor: number }
  | { kind: "reset" };

export interface FrameStats {
  frame_ms: number;
  gpu: string;
}

export interface SelectionSummary {
  atoms: number;
  residues: number;
}

export interface MeasurementSummary {
  name: string;
  label: string;
}

/** A measurement label at a physical pixel position, or null when off screen. */
export interface Label {
  name: string;
  text: string;
  position: [number, number] | null;
}

export interface CommandResult {
  message: string;
  canonical: string | null;
  atoms: number | null;
  /** The "sele" selection after the command. */
  selection: SelectionSummary | null;
  measurements: MeasurementSummary[];
  can_undo: boolean;
  can_redo: boolean;
}

export interface PickInfo {
  /** 1-based atom index, as the `index` selection keyword expects. */
  index: number;
  label: string;
  chain: string;
  residue: string;
  atom: string;
  element: string;
  b_factor: number;
}

export interface Preset {
  name: string;
  commands: string;
}

export const loadStructure = (path: string) => invoke<StructureInfo>("load_structure", { path });
/** Downloads a PDB entry from RCSB (cached on disk) and loads it. */
export const fetchStructure = (id: string) => invoke<StructureInfo>("fetch_structure", { id });
export const camera = (action: CameraAction) => invoke<void>("camera", { action });
export const presets = () => invoke<Preset[]>("presets");
/** The atom under a point in CSS pixels, or null. */
export const pick = (x: number, y: number) =>
  invoke<PickInfo | null>("pick", { x: x * devicePixelRatio, y: y * devicePixelRatio });
/** Runs a command-bar line; `dryRun` reports what it would do without changing anything. */
export const runCommand = (text: string, dryRun: boolean) =>
  invoke<CommandResult>("run_command", { text, dryRun });
export const startupFile = () => invoke<string | null>("startup_file");
