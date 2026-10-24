<script lang="ts">
  // PyMOL-style command line. While typing, each line is dry-run for live feedback (atom count,
  // canonical selection, or an error with carets under the offending text); Enter runs it.
  import type { CommandResult, SelectionError } from "./api";
  import { consoleState, execute, type Outcome } from "./console.svelte";

  let text = $state("");
  let result: Outcome | null = $state(null);
  let historyIndex = -1;
  let timer: ReturnType<typeof setTimeout> | undefined;

  function oninput() {
    clearTimeout(timer);
    const input = text;
    timer = setTimeout(async () => {
      const r = await execute(input, { dryRun: true });
      if (input === text) result = r;
    }, 60);
  }

  async function onkeydown(e: KeyboardEvent) {
    const history = consoleState.history;
    if (e.key === "Enter" && text.trim()) {
      // Errors stay inline (the text is kept so it can be fixed in place), not in the history.
      const r = await execute(text, { logErrors: false });
      if (r?.ok) {
        text = "";
        result = null;
      } else {
        result = r;
      }
      historyIndex = -1;
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

  const preview = (r: CommandResult) =>
    r.canonical && r.atoms !== null ? `${r.atoms.toLocaleString("en-US")} atoms · ${r.canonical}` : r.message;
</script>

<div class="command panel">
  {#if consoleState.history.length}
    <ol class="history mono">
      {#each consoleState.history as entry}
        <li><span class="echo">› {entry.input}</span><span class:error={!entry.ok}>{entry.output}</span></li>
      {/each}
    </ol>
  {/if}
  <div class="row">
    <span class="prompt mono">rsmol›</span>
    <div class="field">
      <input
        class="mono"
        bind:value={text}
        {oninput}
        {onkeydown}
        spellcheck="false"
        autocomplete="off"
        placeholder="byres (organic around 5) and polymer   ·   show sticks, sele   ·   color coral, chain A"
      />
      {#if result && !result.ok}
        <div class="carets mono" aria-hidden="true">{carets(result.error)}</div>
      {/if}
    </div>
  </div>
  {#if result}
    <div class="status mono" class:error={!result.ok}>
      {result.ok ? preview(result.value) : result.error.message}
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
