<script lang="ts">
  import { fade, scale } from "svelte/transition";

  import Portal from "./Portal.svelte";

  /** Whether the dialog is shown. When false nothing is rendered. */
  export let open = false;
  /** Called on close (Esc, backdrop click). Consumers also wire their own
   *  close buttons to this. */
  export let onClose: () => void = () => {};
  /** Stacking order. Layered dialogs (e.g. a prompt above the server form)
   *  pass a higher z than the dialog beneath them — this replaces the old
   *  headlessui dialog-stack context with plain z-index ordering. */
  export let z = 50;
  /** Width/layout classes for the panel wrapper (defaults full width; consumers
   *  pass e.g. `sm:w-[calc(100%-32px)]`). */
  export let panelClass = "w-full";
  /** Inline style for the panel wrapper (e.g. a pixel `max-width`). */
  export let panelStyle = "";

  function keydown(event: KeyboardEvent) {
    if (open && event.key === "Escape") {
      event.preventDefault();
      onClose();
    }
  }
</script>

<svelte:window on:keydown={keydown} />

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
        on:click={onClose}
        transition:fade={{ duration: 150 }}>&#8203;</button
      >
      <div
        class="{panelClass} relative"
        style={panelStyle}
        role="dialog"
        aria-modal="true"
        transition:scale={{ start: 0.95, duration: 150 }}
      >
        <slot />
      </div>
    </div>
  </Portal>
{/if}
