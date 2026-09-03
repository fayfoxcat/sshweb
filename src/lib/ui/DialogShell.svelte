<script lang="ts">
  import { XIcon } from "svelte-feather-icons";

  import { lang, t } from "$lib/i18n";
  export let title = "";
  export let message = "";
  export let confirmText = "";
  export let danger = false;
  /** Optional secondary action between cancel and confirm (e.g. "跳过"). */
  export let middleText = "";
  export let onConfirm: () => void;
  export let onMiddle: () => void = () => {};
  export let onCancel: () => void;
</script>

<div
  class="relative w-full max-w-sm rounded-lg border border-zinc-700 bg-zinc-900 p-5 shadow-2xl"
>
  <div class="flex items-start justify-between">
    <h3 class="text-base font-medium text-zinc-100">{title}</h3>
    <button
      class="rounded p-1 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200"
      on:click={onCancel}
      title={t($lang, "common.close")}
    >
      <XIcon size="16" />
    </button>
  </div>
  {#if message}
    <p class="mt-2 text-sm text-zinc-400">{message}</p>
  {/if}
  <slot />
  <div class="mt-5 flex justify-end gap-2">
    <button
      class="rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700"
      on:click={onCancel}
    >
      {t($lang, "common.cancel")}
    </button>
    {#if middleText}
      <button
        class="rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700"
        on:click={onMiddle}
      >
        {middleText}
      </button>
    {/if}
    <button
      class="rounded-md px-3 py-1.5 text-sm font-medium text-white transition-colors {danger
        ? 'bg-red-700 hover:bg-red-600'
        : 'bg-indigo-700 hover:bg-indigo-600'}"
      on:click={onConfirm}
    >
      {confirmText}
    </button>
  </div>
</div>
