<script lang="ts">
  import type { Snippet } from "svelte";

  import { XIcon } from "$lib/icons";
  import { lang, t } from "$lib/i18n";

  interface Props {
    title?: string;
    message?: string;
    confirmText?: string;
    danger?: boolean;
    /** Optional secondary action between cancel and confirm (e.g. "跳过"). */
    middleText?: string;
    onConfirm?: () => void;
    onMiddle?: () => void;
    onCancel?: () => void;
    children?: Snippet;
  }

  let {
    title = "",
    message = "",
    confirmText = "",
    danger = false,
    middleText = "",
    onConfirm = () => {},
    onMiddle = () => {},
    onCancel = () => {},
    children,
  }: Props = $props();
</script>

<div
  class="relative w-full max-w-sm rounded-lg border border-zinc-700 bg-zinc-900 p-5 shadow-2xl"
>
  <div class="flex items-start justify-between">
    <h3 class="text-base font-medium text-zinc-100">{title}</h3>
    <button
      class="rounded p-1 text-zinc-400 hover:bg-zinc-700 hover:text-zinc-200"
      onclick={onCancel}
      title={t($lang, "common.close")}
    >
      <XIcon size="16" />
    </button>
  </div>
  {#if message}
    <p class="mt-2 text-sm text-zinc-400">{message}</p>
  {/if}
  {@render children?.()}
  <div class="mt-5 flex justify-end gap-2">
    <button
      class="rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700"
      onclick={onCancel}
    >
      {t($lang, "common.cancel")}
    </button>
    {#if middleText}
      <button
        class="rounded-md px-3 py-1.5 text-sm text-zinc-300 transition-colors hover:bg-zinc-700"
        onclick={onMiddle}
      >
        {middleText}
      </button>
    {/if}
    <button
      class="rounded-md px-3 py-1.5 text-sm font-medium text-white transition-colors {danger
        ? 'bg-red-700 hover:bg-red-600'
        : 'bg-indigo-700 hover:bg-indigo-600'}"
      onclick={onConfirm}
    >
      {confirmText}
    </button>
  </div>
</div>
