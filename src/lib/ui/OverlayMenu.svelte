<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import { XIcon } from "$lib/icons";
  import { lang, t } from "$lib/i18n";
  import Dialog from "./Dialog.svelte";

  const dispatch = createEventDispatcher<{ close: void }>();

  export let title: string;
  export let description = "";
  export let showCloseButton = false;
  export let maxWidth: number = 768; // screen-md
  export let open: boolean;

  function close() {
    dispatch("close");
  }
</script>

<Dialog
  {open}
  onClose={close}
  panelClass="w-full sm:w-[calc(100%-32px)]"
  panelStyle="max-width:{maxWidth}px"
>
  <div
    class="relative bg-[#111] sm:border border-zinc-800 px-6 py-10 sm:py-6
     h-screen sm:h-auto max-h-screen sm:rounded-lg overflow-y-auto no-bar"
  >
    {#if showCloseButton}
      <button
        class="absolute top-4 right-4 p-1 rounded hover:bg-zinc-700 active:bg-indigo-700 transition-colors"
        aria-label={t($lang, "common.close")}
        on:click={close}
      >
        <XIcon class="h-5 w-5" />
      </button>
    {/if}

    <div class="mb-8 text-center">
      <h2 class="text-xl font-medium mb-2">{title}</h2>
      {#if description}
        <p class="text-zinc-400">{description}</p>
      {/if}
    </div>

    <slot />
  </div>
</Dialog>

<style>
  .no-bar {
    scrollbar-width: none;
  }
  .no-bar::-webkit-scrollbar {
    display: none;
  }
</style>
