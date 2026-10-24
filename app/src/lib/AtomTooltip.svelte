<script lang="ts">
  // Follows the pointer while it is over an atom.
  import { consoleState } from "./console.svelte";

  const OFFSET = 14;
</script>

{#if consoleState.hover}
  {@const { info, x, y } = consoleState.hover}
  <div class="tooltip panel" style:left={`${x + OFFSET}px`} style:top={`${y + OFFSET}px`} role="tooltip">
    <div class="title">
      <span class="chain mono">{info.chain || "–"}</span>
      <strong>{info.residue}</strong>
    </div>
    <div class="detail mono">
      {info.atom} · {info.element} · B {info.b_factor.toFixed(1)}
    </div>
    <div class="hint">click to select · shift-click to add · double-click to zoom</div>
  </div>
{/if}

<style>
  .tooltip {
    position: absolute;
    z-index: 10;
    padding: 7px 10px;
    pointer-events: none;
    border-radius: 9px;
    white-space: nowrap;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .chain {
    font-size: 11px;
    padding: 0 5px;
    border-radius: 4px;
    background: rgba(95, 186, 168, 0.18);
    color: var(--accent);
  }
  strong {
    font-weight: 600;
  }
  .detail {
    margin-top: 2px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .hint {
    margin-top: 4px;
    font-size: 10.5px;
    color: var(--text-faint);
  }
</style>
