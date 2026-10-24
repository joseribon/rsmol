<script lang="ts">
  // PyMOL-style command line. For now it only parses selections, showing the canonical form
  // or an error with carets under the offending text, live as you type.
  import { parseSelection, type SelectionError } from "./api";

  type Result = { ok: true; canonical: string } | { ok: false; error: SelectionError };

  let text = $state("");
  let result: Result | null = $state(null);
  let history: { input: string; output: string; ok: boolean }[] = $state([]);
  let historyIndex = -1;
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function check(input: string): Promise<Result | null> {
    if (!input.trim()) return null;
    try {
      return { ok: true, canonical: await parseSelection(input) };
    } catch (error) {
      return { ok: false, error: error as SelectionError };
    }
  }

  function oninput() {
    clearTimeout(timer);
    const input = text;
    timer = setTimeout(async () => {
      const r = await check(input);
      if (input === text) result = r;
    }, 60);
  }

  async function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && text.trim()) {
      const r = await check(text);
      if (!r) return;
      history = [
        ...history.slice(-4),
        r.ok
          ? { input: text, output: `parsed as ${r.canonical}  (evaluation not wired up yet)`, ok: true }
          : { input: text, output: r.error.message, ok: false },
      ];
      historyIndex = -1;
      text = "";
      result = null;
    } else if (e.key === "ArrowUp" && history.length) {
      e.preventDefault();
      historyIndex = historyIndex < 0 ? history.length - 1 : Math.max(0, historyIndex - 1);
      text = history[historyIndex].input;
      oninput();
    } else if (e.key === "Escape") {
      text = "";
      result = null;
    }
  }

  // Carets line up with the input because both use the same monospace font and padding.
  const carets = (err: SelectionError) =>
    " ".repeat(err.start) + "^".repeat(Math.max(1, err.end - err.start));
</script>

<div class="command panel">
  {#if history.length}
    <ol class="history mono">
      {#each history as entry}
        <li><span class="echo">› {entry.input}</span><span class:error={!entry.ok}>{entry.output}</span></li>
      {/each}
    </ol>
  {/if}
  <div class="row">
    <span class="prompt mono">sele›</span>
    <div class="field">
      <input
        class="mono"
        bind:value={text}
        {oninput}
        {onkeydown}
        spellcheck="false"
        autocomplete="off"
        placeholder="byres organic around 5 and not solvent"
      />
      {#if result && !result.ok}
        <div class="carets mono" aria-hidden="true">{carets(result.error)}</div>
      {/if}
    </div>
  </div>
  {#if result}
    <div class="status mono" class:error={!result.ok}>
      {result.ok ? `= ${result.canonical}` : result.error.message}
    </div>
  {/if}
</div>

<style>
  .command {
    padding: 10px 14px;
    user-select: text;
    -webkit-user-select: text;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .prompt {
    color: var(--accent);
    font-size: 13px;
  }
  .field {
    position: relative;
    flex: 1;
  }
  input,
  .carets {
    font-size: 13px;
    padding: 0;
    letter-spacing: 0;
  }
  input {
    width: 100%;
    background: none;
    border: none;
    outline: none;
    color: var(--text);
  }
  input::placeholder {
    color: var(--text-faint);
  }
  .carets {
    position: absolute;
    left: 0;
    top: 1.15em;
    color: var(--danger);
    white-space: pre;
    pointer-events: none;
    line-height: 1;
  }
  .status {
    margin-top: 8px;
    font-size: 12px;
    color: var(--text-muted);
    white-space: pre-wrap;
  }
  .history {
    list-style: none;
    margin: 0 0 8px;
    padding: 0 0 8px;
    border-bottom: 1px solid var(--panel-border);
    font-size: 12px;
    color: var(--text-muted);
  }
  .history li {
    display: flex;
    flex-direction: column;
    padding: 2px 0;
  }
  .echo {
    color: var(--text);
  }
  .error {
    color: var(--danger);
  }
</style>
