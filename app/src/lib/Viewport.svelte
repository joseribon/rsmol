<script lang="ts">
  // Transparent input layer over the native viewport. Pointer deltas are accumulated and sent
  // once per animation frame, so IPC traffic stays bounded no matter how fast the mouse moves.
  import { onMount } from "svelte";
  import { camera } from "./api";

  let el: HTMLDivElement;
  let mode: "orbit" | "pan" | null = null;
  const pending = { orbit: [0, 0], pan: [0, 0], zoom: 1 };
  let scheduled = false;

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

  function onpointerdown(e: PointerEvent) {
    mode = e.button === 0 && !e.shiftKey ? "orbit" : "pan";
    el.setPointerCapture(e.pointerId);
  }

  function onpointermove(e: PointerEvent) {
    if (!mode) return;
    const delta = pending[mode];
    delta[0] += e.movementX;
    delta[1] += e.movementY;
    schedule();
  }

  function onpointerup(e: PointerEvent) {
    mode = null;
    el.releasePointerCapture(e.pointerId);
  }

  onMount(() => {
    // Registered manually: wheel listeners must be non-passive to stop page scrolling.
    const onwheel = (e: WheelEvent) => {
      e.preventDefault();
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
  bind:this={el}
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  onpointercancel={onpointerup}
  ondblclick={() => camera({ kind: "reset" })}
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
  .dragging {
    cursor: grabbing;
  }
</style>
