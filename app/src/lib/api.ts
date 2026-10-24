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

export interface CommandResult {
  message: string;
  canonical: string | null;
  atoms: number | null;
  /** The "sele" selection after the command. */
  selection: SelectionSummary | null;
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
/** Runs a command-bar line; `dryRun` reports what it would do without changing anything. */
export const runCommand = (text: string, dryRun: boolean) =>
  invoke<CommandResult>("run_command", { text, dryRun });
export const startupFile = () => invoke<string | null>("startup_file");
