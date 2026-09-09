<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import {
    CheckCircleIcon,
    HelpCircleIcon,
    InfoIcon,
    XCircleIcon,
    XIcon,
  } from "$lib/icons";
  import { tr } from "$lib/i18n";

  const dispatch = createEventDispatcher<{ action: void; dismiss: void }>();

  /** The kind of toast to display. */
  export let kind: "info" | "success" | "error" = "info";

  /** The message to display inside the toast. */
  export let message: string;

  /** An optional action to provide as a button on the toast. */
  export let action = "";
</script>

<div class="toast-box">
  {#if kind === "info"}
    <InfoIcon class="w-5 h-5 text-lime-300 flex-shrink-0" />
  {:else if kind === "success"}
    <CheckCircleIcon class="w-5 h-5 text-green-300 flex-shrink-0" />
  {:else if kind === "error"}
    <XCircleIcon class="w-5 h-5 text-red-300 flex-shrink-0" />
  {:else}
    <HelpCircleIcon class="w-5 h-5 text-lime-300 flex-shrink-0" />
  {/if}

  <p class="ml-3 min-w-0 flex-1">
    {message}
  </p>

  {#if action}
    <button
      class="h-5 ml-3 shrink-0 px-2 flex items-center text-xs border rounded-md border-zinc-400 hover:border-zinc-200 hover:text-white transition-colors"
      type="button"
      on:click={() => dispatch("action")}
    >
      {action}
    </button>
  {/if}

  <!-- Explicit dismiss affordance (real button) — the toast itself is no longer
       click-anywhere-to-dismiss. -->
  <button
    type="button"
    class="ml-3 shrink-0 rounded p-1 text-zinc-500 transition-colors hover:bg-zinc-700 hover:text-zinc-100"
    aria-label={tr("common.close")}
    title={tr("common.close")}
    on:click={() => dispatch("dismiss")}
  >
    <XIcon class="h-4 w-4" />
  </button>
</div>

<style lang="postcss">
  .toast-box {
    @apply border border-zinc-700 bg-zinc-900/80 backdrop-blur-sm;
    @apply p-4 rounded-md flex items-start pointer-events-auto;
    @apply text-sm;
  }
</style>
