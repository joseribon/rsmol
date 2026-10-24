// Shared command execution: the command bar, preset buttons and selection card all run commands
// through here, so every action lands in one history showing the PyMOL syntax behind it.
import { runCommand, type CommandResult, type SelectionError, type SelectionSummary } from "./api";

export type Outcome = { ok: true; value: CommandResult } | { ok: false; error: SelectionError };
export interface Entry {
  input: string;
  output: string;
  ok: boolean;
}

export const consoleState = $state({
  history: [] as Entry[],
  /** The current "sele", or null when nothing is selected. */
  selection: null as SelectionSummary | null,
});

const HISTORY_LIMIT = 5;

/** Runs `input`. Real runs are logged; failures too when `logErrors` (button clicks). */
export async function execute(input: string, { dryRun = false, logErrors = true } = {}): Promise<Outcome | null> {
  if (!input.trim()) return null;
  let outcome: Outcome;
  try {
    outcome = { ok: true, value: await runCommand(input, dryRun) };
  } catch (error) {
    outcome = { ok: false, error: error as SelectionError };
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

function log(entry: Entry) {
  consoleState.history = [...consoleState.history.slice(-(HISTORY_LIMIT - 1)), entry];
}

/** Clears per-structure state when a new structure is loaded. */
export function resetConsole() {
  consoleState.selection = null;
}
