<script lang="ts">
  // Measurement labels, positioned each frame from anchors projected by the render thread
  // (native) or the web renderer.
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import type { Label } from "./api";
  import { webView } from "./webview.svelte";

  let nativeLabels: Label[] = $state([]);
  // The webview viewport projects its own labels.
  const labels = $derived(webView.active ? webView.labels : nativeLabels);

  onMount(() => {
    const unlisten = listen<Label[]>("viewport-labels", (e) => (nativeLabels = e.payload));
    return () => unlisten.then((f) => f());
  });
</script>

{#each labels as label (label.name)}
  {#if label.position}
    <div
      class="label mono"
      style:left={`${label.position[0] / devicePixelRatio}px`}
      style:top={`${label.position[1] / devicePixelRatio}px`}
      title={label.name}
    >
      {label.text}
    </div>
  {/if}
{/each}

<style>
  .label {
    position: absolute;
    transform: translate(-50%, -50%);
    padding: 1px 6px;
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    color: #1a1406;
    background: rgba(245, 196, 84, 0.92);
    box-shadow: 0 1px 6px rgba(0, 0, 0, 0.4);
    pointer-events: none;
    white-space: nowrap;
  }
</style>
