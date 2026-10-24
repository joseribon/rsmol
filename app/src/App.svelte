<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { loadStructure, presets, startupFile, type FrameStats, type Preset, type StructureInfo } from "./lib/api";
  import { execute, resetConsole } from "./lib/console.svelte";
  import { SCHEMES } from "./lib/palette";
  import Viewport from "./lib/Viewport.svelte";
  import CommandBar from "./lib/CommandBar.svelte";
  import SelectionCard from "./lib/SelectionCard.svelte";

  let info: StructureInfo | null = $state(null);
  let stats: FrameStats | null = $state(null);
  let loading = $state(false);
  let error: string | null = $state(null);
  let dropping = $state(false);
  let looks: Preset[] = $state([]);
  // The first preset matches the default display a structure loads with.
  let activeLook = $state(0);

  async function open(path: string) {
    loading = true;
    error = null;
    try {
      info = await loadStructure(path);
      activeLook = 0;
      resetConsole();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function applyLook(index: number) {
    const outcome = await execute(looks[index].commands);
    if (outcome?.ok) activeLook = index;
  }

  onMount(() => {
    presets().then((p) => (looks = p));
    startupFile().then((path) => {
      if (path) open(path);
    });
    const unlisten = [
      listen<FrameStats>("viewport-frame", (e) => (stats = e.payload)),
      getCurrentWebview().onDragDropEvent((e) => {
        const { type } = e.payload;
        dropping = type === "enter" || type === "over";
        if (e.payload.type === "drop" && e.payload.paths.length) open(e.payload.paths[0]);
      }),
    ];
    return () => unlisten.forEach((p) => p.then((f) => f()));
  });

  const fmt = (n: number) => n.toLocaleString("en-US");
</script>

<Viewport />

<header class="titlebar" data-tauri-drag-region>
  <span class="brand" data-tauri-drag-region>rsmol</span>
  {#if info}<span class="crumb" data-tauri-drag-region>{info.name}</span>{/if}
</header>

{#if info}
  <aside class="info panel">
    <h2>{info.name}</h2>
    <dl>
      <dt>Atoms</dt><dd>{fmt(info.atoms)}</dd>
      <dt>Residues</dt><dd>{fmt(info.residues)}</dd>
      <dt>States</dt><dd>{fmt(info.states)}</dd>
      <dt>Parsed</dt><dd>{info.parse_ms.toFixed(0)} ms</dd>
    </dl>

    <h3>Style</h3>
    <div class="segmented" role="radiogroup" aria-label="Style">
      {#each looks as look, i}
        <button
          role="radio"
          aria-checked={activeLook === i}
          class:active={activeLook === i}
          title={look.commands}
          onclick={() => applyLook(i)}>{look.name}</button
        >
      {/each}
    </div>

    <h3>Color</h3>
    <div class="schemes">
      {#each SCHEMES as scheme}
        <button title={`${scheme.hint} — color ${scheme.command}`} onclick={() => execute(`color ${scheme.command}`)}>
          {scheme.label}
        </button>
      {/each}
    </div>

    <h3>Chains</h3>
    <div class="chains">
      {#each info.chains.slice(0, 40) as chain}
        <button class="chip mono" title={`select chain ${chain}`} onclick={() => execute(`select chain ${chain || "''"}`)}>
          {chain || "–"}
        </button>
      {/each}
      {#if info.chains.length > 40}<span class="more">+{info.chains.length - 40}</span>{/if}
    </div>
  </aside>
{/if}

{#if !info || dropping}
  <div class="empty" class:dropping>
    <div class="drop-target">
      {#if loading}
        Loading…
      {:else}
        <strong>Drop a structure file</strong>
        <span>.cif · .mmcif · .pdb · .ent</span>
      {/if}
    </div>
  </div>
{/if}

{#if error}
  <div class="toast panel" role="alert">
    {error}
    <button onclick={() => (error = null)} aria-label="Dismiss">×</button>
  </div>
{/if}

<footer>
  <div class="command-slot">
    <SelectionCard />
    <CommandBar />
  </div>
  {#if stats}
    <div class="stats panel mono" title={stats.gpu}>{stats.frame_ms.toFixed(2)} ms · {stats.gpu}</div>
  {/if}
</footer>

<style>
  .titlebar {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 38px;
    display: flex;
    align-items: center;
    gap: 10px;
    /* Clear the macOS traffic lights drawn over the content by the Overlay title bar. */
    padding-left: 84px;
  }
  .brand {
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .crumb {
    color: var(--text-muted);
  }
  .crumb::before {
    content: "/";
    margin-right: 10px;
    color: var(--text-faint);
  }

  .info {
    position: absolute;
    top: 48px;
    left: 12px;
    width: 220px;
    padding: 14px 16px;
  }
  h2 {
    margin: 0 0 10px;
    font-size: 15px;
    font-weight: 600;
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin: 0;
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  button {
    font: inherit;
    cursor: pointer;
  }
  .segmented {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 3px;
    border-radius: 9px;
    background: rgba(255, 255, 255, 0.04);
  }
  .segmented button {
    padding: 5px 8px;
    border: none;
    border-radius: 6px;
    background: none;
    color: var(--text-muted);
    text-align: left;
  }
  .segmented button:hover {
    color: var(--text);
  }
  .segmented button.active {
    background: rgba(95, 186, 168, 0.16);
    color: var(--text);
  }
  .schemes {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .schemes button {
    padding: 3px 8px;
    border-radius: 6px;
    border: 1px solid var(--panel-border);
    background: rgba(255, 255, 255, 0.04);
    color: var(--text-muted);
    font-size: 12px;
  }
  .schemes button:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.09);
  }
  .chains {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    font-size: 11px;
    padding: 1px 6px;
    border: none;
    border-radius: 6px;
    background: rgba(255, 255, 255, 0.06);
    color: var(--text-muted);
  }
  .chip:hover {
    background: rgba(255, 255, 255, 0.12);
    color: var(--text);
  }
  .more {
    font-size: 11px;
    color: var(--text-faint);
  }

  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    pointer-events: none;
  }
  .drop-target {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 36px 56px;
    border: 1.5px dashed rgba(255, 255, 255, 0.14);
    border-radius: 18px;
    color: var(--text-muted);
    transition: border-color 120ms, background 120ms;
  }
  .drop-target strong {
    color: var(--text);
    font-size: 15px;
    font-weight: 600;
  }
  .drop-target span {
    font-size: 12px;
  }
  .dropping .drop-target {
    border-color: var(--accent);
    background: rgba(95, 186, 168, 0.08);
  }

  .toast {
    position: absolute;
    top: 48px;
    left: 50%;
    transform: translateX(-50%);
    max-width: 60%;
    padding: 10px 14px;
    color: var(--danger);
    display: flex;
    gap: 12px;
    align-items: start;
  }
  .toast button {
    background: none;
    border: none;
    color: var(--text-muted);
    font-size: 16px;
  }

  footer {
    position: absolute;
    left: 12px;
    right: 12px;
    bottom: 12px;
    display: flex;
    align-items: end;
    gap: 12px;
    pointer-events: none;
  }
  .command-slot {
    flex: 1;
    max-width: 760px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    pointer-events: auto;
  }
  .stats {
    position: absolute;
    right: 0;
    bottom: 0;
    padding: 6px 10px;
    font-size: 11px;
    color: var(--text-muted);
    pointer-events: auto;
  }
</style>
