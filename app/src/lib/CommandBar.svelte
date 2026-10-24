<script lang="ts">
  // PyMOL-style command line. While typing, each line is dry-run for live feedback (atom count,
  // canonical selection, or an error with carets under the offending text); Enter runs it.
  import type { CommandResult, SelectionError } from "./api";
  import { consoleState, execute, type Outcome } from "./console.svelte";

  let text = $state("");
  let result: Outcome | null = $state(null);
  let historyIndex = -1;
  let timer: ReturnType<typeof setTimeout> | undefined;

  // Collapsed by default so the history never covers the structure; the choice is remembered.
  const HISTORY_KEY = "rsmol.historyExpanded";
  let historyExpanded = $state(readFlag(HISTORY_KEY));
  let historyList: HTMLOListElement | undefined = $state();

  // Keep the newest entry in view when the list scrolls.
  $effect(() => {
    void consoleState.history.length;
    historyList?.scrollTo({ top: historyList.scrollHeight });
  });

  function readFlag(key: string): boolean {
    try {
      return localStorage.getItem(key) === "1";
    } catch {
      return false;
    }
  }

  function toggleHistory() {
    historyExpanded = !historyExpanded;
    try {
      localStorage.setItem(HISTORY_KEY, historyExpanded ? "1" : "0");
    } catch {
      // Storage unavailable: the toggle still works for this session.
    }
  }

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
  {#if historyExpanded && consoleState.history.length}
    <ol class="history mono" id="command-history" bind:this={historyList}>
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
    {#if consoleState.history.length}
      <button
        class="history-toggle"
        class:open={historyExpanded}
        aria-expanded={historyExpanded}
        aria-controls="command-history"
        title={historyExpanded ? "Hide history" : "Show history"}
        onclick={toggleHistory}
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M2 6.5 5 3.5 8 6.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
        </svg>
        <span class="mono">{consoleState.history.length}</span>
      </button>
    {/if}
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
  .history-toggle {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 7px;
    border: 1px solid var(--panel-border);
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.04);
    color: var(--text-muted);
    font-size: 11px;
    cursor: pointer;
    align-self: center;
  }
  .history-toggle:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.09);
  }
  .history-toggle svg {
    transition: transform 140ms ease;
  }
  /* Chevron points up (expand) when collapsed and down (collapse) when open. */
  .history-toggle.open svg {
    transform: rotate(180deg);
  }
  .history {
    list-style: none;
    margin: 0 0 8px;
    padding: 0 0 8px;
    /* Scroll rather than grow over the structure. */
    max-height: 132px;
    overflow-y: auto;
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
