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

export type Representation = "spheres" | "sticks" | "cartoon" | "ligands";

export interface FrameStats {
  frame_ms: number;
  gpu: string;
}

export const loadStructure = (path: string) => invoke<StructureInfo>("load_structure", { path });
export const camera = (action: CameraAction) => invoke<void>("camera", { action });
export const setRepresentation = (representation: Representation) =>
  invoke<void>("set_representation", { representation });
export interface CommandResult {
  message: string;
  canonical: string | null;
  atoms: number | null;
}

/** Runs a command-bar line; `dryRun` reports what it would do without changing anything. */
export const runCommand = (text: string, dryRun: boolean) =>
  invoke<CommandResult>("run_command", { text, dryRun });
export const startupFile = () => invoke<string | null>("startup_file");
