<script lang="ts">
  import { sidebarDragging, sidebarWidth, sidebarWidthCss } from "./dragResize";

  /** Pointer handlers produced by `createSidebarResize` (dragResize.ts). */
  export let resize: {
    onStart(e: PointerEvent): void;
    onMove(e: PointerEvent): void;
    onEnd(e: PointerEvent): void;
  };
  /** Whether the resize handle is shown (the file panel hides it when closed). */
  export let showHandle: boolean;
  /** Whether the panel is visible (its width collapses to 0 when closed). */
  export let open = true;

  $: widthCss = sidebarWidthCss($sidebarWidth, open);
  $: dragging = $sidebarDragging;
</script>

<aside
  class="relative flex shrink-0 flex-col overflow-hidden border-r border-zinc-800 bg-zinc-900/60"
  style:width={widthCss}
  class:cursor-col-resize={dragging}
  on:mousedown
  on:mouseup
  on:auxclick
  on:dragover
  on:drop
>
  <slot />
  {#if showHandle}
    <div
      class="absolute inset-y-0 right-0 w-1.5 cursor-col-resize hover:bg-indigo-500/40 active:bg-indigo-500/60"
      on:pointerdown={resize.onStart}
      on:pointermove={resize.onMove}
      on:pointerup={resize.onEnd}
      on:pointercancel={resize.onEnd}
    />
  {/if}
</aside>
