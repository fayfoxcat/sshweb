<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import type { Snippet } from "svelte";

  import Portal from "./Portal.svelte";

  interface Props {
    /** Whether the dialog is shown. When false nothing is rendered. */
    open?: boolean;
    /** Called on close (Esc, backdrop click). */
    onClose?: () => void;
    /** Stacking order (replaces the old headlessui dialog-stack context). */
    z?: number;
    /** Width/layout classes for the panel wrapper. */
    panelClass?: string;
    /** Inline style for the panel wrapper (e.g. pixel max-width). */
    panelStyle?: string;
    children?: Snippet;
  }

  let {
    open = false,
    onClose = () => {},
    z = 50,
    panelClass = "w-full",
    panelStyle = "",
    children,
  }: Props = $props();

  function keydown(event: KeyboardEvent) {
    if (open && event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={keydown} />

{#if open}
  <Portal>
    <div
      class="fixed inset-0 flex items-center justify-center"
      style="z-index:{z}"
    >
      <!-- Backdrop (a real <button> so the click is keyboard-reachable and no
           a11y handler is required). Clicking it closes the dialog; the panel
           sits above it so panel clicks never reach it. -->
      <button
        type="button"
        class="absolute inset-0 cursor-default bg-black/40"
        style="border:0;padding:0"
        aria-label="关闭"
        tabindex="-1"
        onclick={onClose}
        transition:fade={{ duration: 150 }}>&#8203;</button
      >
      <div
        class="{panelClass} relative"
        style={panelStyle}
        role="dialog"
        aria-modal="true"
        transition:scale={{ start: 0.95, duration: 150 }}
      >
        {@render children?.()}
      </div>
    </div>
  </Portal>
{/if}
