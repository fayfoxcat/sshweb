<script lang="ts">
  import { sidebarDragging, sidebarWidth, sidebarWidthCss } from "./dragResize";
  import type { Snippet } from "svelte";

  interface Props {
    /** Pointer handlers produced by `createSidebarResize` (dragResize.ts). */
    resize: {
      onStart(e: PointerEvent): void;
      onMove(e: PointerEvent): void;
      onEnd(e: PointerEvent): void;
    };
    /** Whether the resize handle is shown (the file panel hides it when closed). */
    showHandle: boolean;
    /** Whether the panel is visible (its width collapses to 0 when closed). */
    open?: boolean;
    children?: Snippet;
  }

  let { resize, showHandle, open = true, children }: Props = $props();

  let widthCss = $derived(sidebarWidthCss($sidebarWidth, open));
  let dragging = $derived($sidebarDragging);
</script>

<aside
  class="relative flex shrink-0 flex-col overflow-hidden border-r border-zinc-800 bg-zinc-900/60"
  style:width={widthCss}
  class:cursor-col-resize={dragging}
>
  {@render children?.()}
  {#if showHandle}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="absolute inset-y-0 right-0 w-1.5 cursor-col-resize hover:bg-indigo-500/40 active:bg-indigo-500/60"
      onpointerdown={resize.onStart}
      onpointermove={resize.onMove}
      onpointerup={resize.onEnd}
      onpointercancel={resize.onEnd}
    ></div>
  {/if}
</aside>
