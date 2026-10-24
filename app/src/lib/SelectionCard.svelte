<script lang="ts">
  // One-click actions for the current selection. Each button runs the equivalent command, so the
  // history above the command bar teaches the syntax.
  import { consoleState, execute } from "./console.svelte";
  import { SWATCHES } from "./palette";

  const actions: { label: string; command: string; hint: string }[] = [
    { label: "Zoom", command: "zoom sele", hint: "Frame the selection" },
    { label: "Sticks", command: "show sticks, sele", hint: "Add sticks" },
    { label: "Spheres", command: "show spheres, sele", hint: "Add van der Waals spheres" },
    { label: "Cartoon", command: "show cartoon, sele", hint: "Add cartoon" },
    { label: "Surface", command: "show surface, sele", hint: "Add a molecular surface" },
    { label: "Hide", command: "hide everything, sele", hint: "Hide every representation" },
  ];

  const fmt = (n: number) => n.toLocaleString("en-US");
</script>

{#if consoleState.selection}
  {@const sel = consoleState.selection}
  <section class="card panel" aria-label="Selection">
    <header>
      <span class="dot" aria-hidden="true"></span>
      <strong>sele</strong>
      <span class="counts">{fmt(sel.atoms)} atoms · {fmt(sel.residues)} residues</span>
      <button class="clear" title="deselect" onclick={() => execute("deselect")}>Clear</button>
    </header>
    <div class="actions">
      {#each actions as a}
        <button title={`${a.hint} — ${a.command}`} onclick={() => execute(a.command)}>{a.label}</button>
      {/each}
    </div>
    <div class="swatches" role="group" aria-label="Color selection">
      {#each SWATCHES as s}
        <button
          class="swatch"
          style:--swatch={s.hex}
          title={`color ${s.name}, sele`}
          aria-label={`Color ${s.name}`}
          onclick={() => execute(`color ${s.name}, sele`)}
        ></button>
      {/each}
      <button class="scheme" title="color byelement, sele" onclick={() => execute("color byelement, sele")}>
        Elements
      </button>
    </div>
  </section>
{/if}

<style>
  .card {
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    animation: rise 160ms ease-out;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #ff5cc8;
    box-shadow: 0 0 8px rgba(255, 92, 200, 0.6);
  }
  strong {
    font-weight: 600;
  }
  .counts {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  button {
    font: inherit;
    color: var(--text);
    cursor: pointer;
  }
  .clear {
    margin-left: auto;
    background: none;
    border: none;
    color: var(--text-muted);
    padding: 2px 4px;
  }
  .clear:hover {
    color: var(--text);
  }
  .actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .actions button,
  .scheme {
    padding: 5px 10px;
    border-radius: 7px;
    border: 1px solid var(--panel-border);
    background: rgba(255, 255, 255, 0.05);
    transition: background 100ms, border-color 100ms;
  }
  .actions button:hover,
  .scheme:hover {
    background: rgba(255, 255, 255, 0.1);
    border-color: rgba(255, 255, 255, 0.16);
  }
  .swatches {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .swatch {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 2px solid rgba(255, 255, 255, 0.1);
    background: var(--swatch);
    padding: 0;
    transition: transform 100ms, border-color 100ms;
  }
  .swatch:hover {
    transform: scale(1.15);
    border-color: rgba(255, 255, 255, 0.5);
  }
  .scheme {
    margin-left: 4px;
    font-size: 12px;
    padding: 3px 9px;
  }
</style>
