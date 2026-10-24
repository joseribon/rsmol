<script lang="ts">
  // Transparent input layer over the native viewport. Drags orbit/pan the camera (deltas are
  // batched per animation frame); hovering shows the atom under the pointer; clicks select.
  import { onMount } from "svelte";
  import { camera, pick } from "./api";
  import { consoleState, execute, measurePick } from "./console.svelte";

  let el: HTMLDivElement;
  let mode: "orbit" | "pan" | null = $state(null);
  const pending = { orbit: [0, 0], pan: [0, 0], zoom: 1 };
  let scheduled = false;
  // Pointer travel since the button went down; below CLICK_SLOP a release counts as a click.
  const CLICK_SLOP = 4;
  let travel = 0;

  function flush() {
    scheduled = false;
    const { orbit, pan, zoom } = pending;
    if (orbit[0] || orbit[1]) camera({ kind: "orbit", dx: orbit[0], dy: orbit[1] });
    if (pan[0] || pan[1]) camera({ kind: "pan", dx: pan[0], dy: pan[1], height: el.clientHeight });
    if (zoom !== 1) camera({ kind: "zoom", factor: zoom });
    pending.orbit = [0, 0];
    pending.pan = [0, 0];
    pending.zoom = 1;
  }

  function schedule() {
    if (!scheduled) {
      scheduled = true;
      requestAnimationFrame(flush);
    }
  }

  // At most one hover pick in flight; the latest position wins.
  let hoverBusy = false;
  let hoverNext: { x: number; y: number } | null = null;
  async function hover(x: number, y: number) {
    if (hoverBusy) {
      hoverNext = { x, y };
      return;
    }
    hoverBusy = true;
    const info = await pick(x, y).catch(() => null);
    hoverBusy = false;
    // Dragging started or the pointer left meanwhile: drop the stale result.
    if (mode === null && hoverNext === null) consoleState.hover = info ? { info, x, y } : null;
    if (hoverNext) {
      const next = hoverNext;
      hoverNext = null;
      hover(next.x, next.y);
    }
  }

  function onpointerdown(e: PointerEvent) {
    mode = e.button === 0 && !e.shiftKey ? "orbit" : "pan";
    travel = 0;
    consoleState.hover = null;
    el.setPointerCapture(e.pointerId);
  }

  function onpointermove(e: PointerEvent) {
    if (!mode) {
      hover(e.clientX, e.clientY);
      return;
    }
    travel += Math.abs(e.movementX) + Math.abs(e.movementY);
    const delta = pending[mode];
    delta[0] += e.movementX;
    delta[1] += e.movementY;
    schedule();
  }

  async function onpointerup(e: PointerEvent) {
    const wasClick = mode !== null && travel < CLICK_SLOP && e.button === 0;
    mode = null;
    el.releasePointerCapture(e.pointerId);
    if (!wasClick) return;
    const info = await pick(e.clientX, e.clientY);
    if (!info) return;
    if (consoleState.measuring) {
      measurePick(info);
      return;
    }
    // Shift-click extends the selection; a plain click replaces it.
    const residue = `byres index ${info.index}`;
    execute(e.shiftKey && consoleState.selection ? `select sele or ${residue}` : `select ${residue}`);
  }

  async function ondblclick(e: MouseEvent) {
    // Two quick picks while measuring are not a request to zoom.
    if (consoleState.measuring) return;
    const info = await pick(e.clientX, e.clientY);
    if (info) execute(`zoom byres index ${info.index}`);
    else camera({ kind: "reset" });
  }

  onMount(() => {
    // Registered manually: wheel listeners must be non-passive to stop page scrolling.
    const onwheel = (e: WheelEvent) => {
      e.preventDefault();
      consoleState.hover = null;
      pending.zoom *= Math.exp(e.deltaY * 0.002);
      schedule();
    };
    el.addEventListener("wheel", onwheel, { passive: false });
    return () => el.removeEventListener("wheel", onwheel);
  });
</script>

<div
  class="viewport"
  class:dragging={mode !== null}
  class:over-atom={consoleState.hover !== null}
  class:measuring={consoleState.measuring !== null}
  bind:this={el}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  onpointercancel={onpointerup}
  onpointerleave={() => {
    hoverNext = null;
    consoleState.hover = null;
  }}
  {ondblclick}
  oncontextmenu={(e) => e.preventDefault()}
  role="application"
  aria-label="Molecular viewport"
></div>

<style>
  .viewport {
    position: absolute;
    inset: 0;
    cursor: grab;
  }
  .over-atom {
    cursor: pointer;
  }
  .measuring.over-atom {
    cursor: crosshair;
  }
  .dragging {
    cursor: grabbing;
  }
</style>
