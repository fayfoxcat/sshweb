<script lang="ts">
  import {
    CheckCircleIcon,
    CheckIcon,
    HelpCircleIcon,
    InfoIcon,
    XCircleIcon,
    XIcon,
  } from "$lib/icons";
  import { tr } from "$lib/i18n";
  import { copyText } from "$lib/clipboard";
  import { pauseToast, resumeToast, type StoredToast } from "$lib/toast";
  import { onDestroy } from "svelte";

  interface Props {
    /** The stored toast to render (its content plus the auto-dismiss deadline,
     *  which hovering this element freezes). */
    toast: StoredToast;
    onDismiss?: () => void;
  }

  let { toast, onDismiss = () => {} }: Props = $props();

  /** Transient result of the click-to-copy action: `null` when idle, and reset
   *  shortly after. Feedback stays **inside** this toast — a second toast
   *  announcing the copy would stack on the first and then vanish by itself. */
  let copied = $state<"ok" | "fail" | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  /** Copy the notice to the clipboard. Bound to the whole box, so every part
   *  of it copies (padding included) and the keyboard activation of the message
   *  button below arrives here too, since a button's synthetic click bubbles. */
  async function copyMessage() {
    copied = (await copyText(toast.message)) ? "ok" : "fail";
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = null), 1200);
  }

  onDestroy(() => clearTimeout(copiedTimer));

  const copiedLabel = $derived(
    copied === "ok"
      ? tr("common.copied")
      : copied === "fail"
        ? tr("common.copyFailed")
        : "",
  );
</script>

<!-- Pointer-only affordance on the container: the actual copy control below is
     a real button, so the action is reachable by keyboard (Enter/Space) as well
     as by pointer — its synthetic click bubbles up to the handler here. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="toast-box"
  title={tr("common.toastCopyHint")}
  onclick={copyMessage}
  onmouseenter={() => pauseToast(toast)}
  onmouseleave={() => resumeToast(toast)}
>
  {#if copied === "ok"}
    <CheckIcon class="w-5 h-5 text-emerald-300 flex-shrink-0" />
  {:else if copied === "fail"}
    <XCircleIcon class="w-5 h-5 text-red-300 flex-shrink-0" />
  {:else if toast.kind === "info"}
    <InfoIcon class="w-5 h-5 text-lime-300 flex-shrink-0" />
  {:else if toast.kind === "success"}
    <CheckCircleIcon class="w-5 h-5 text-green-300 flex-shrink-0" />
  {:else if toast.kind === "error"}
    <XCircleIcon class="w-5 h-5 text-red-300 flex-shrink-0" />
  {:else}
    <HelpCircleIcon class="w-5 h-5 text-lime-300 flex-shrink-0" />
  {/if}

  <!-- The message is a button rather than a paragraph: it makes the copy action
       keyboard-reachable and focusable, which a bare text node is not. Its
       clicks bubble to the box handler above, so there is only one copy path. -->
  <button
    type="button"
    class="ml-3 min-w-0 flex-1 break-words rounded-sm text-left focus-visible:ring-2 focus-visible:ring-indigo-500/50"
  >
    {toast.message}
  </button>

  {#if copiedLabel}
    <span
      class="ml-2 shrink-0 text-xs {copied === 'ok'
        ? 'text-emerald-300'
        : 'text-red-300'}">{copiedLabel}</span
    >
  {/if}

  {#if toast.action}
    <button
      class="h-5 ml-3 shrink-0 px-2 flex items-center text-xs border rounded-md border-zinc-400 hover:border-zinc-200 hover:text-white transition-colors"
      type="button"
      onclick={(event) => {
        event.stopPropagation();
        toast.onAction?.();
      }}
    >
      {toast.action}
    </button>
  {/if}

  <!-- Explicit dismiss affordance (real button) — the toast itself is no longer
       click-anywhere-to-dismiss. -->
  <button
    type="button"
    class="ml-3 shrink-0 rounded p-1 text-zinc-500 transition-colors hover:bg-zinc-700 hover:text-zinc-100"
    aria-label={tr("common.close")}
    title={tr("common.close")}
    onclick={(event) => {
      event.stopPropagation();
      onDismiss();
    }}
  >
    <XIcon class="h-4 w-4" />
  </button>
</div>

<style lang="postcss">
  .toast-box {
    @apply border border-zinc-700 bg-zinc-900/80 backdrop-blur-sm;
    @apply p-4 rounded-md flex items-start pointer-events-auto;
    @apply text-sm;
    /* Nothing may ever paint outside the box. The message wraps long tokens
       (`break-words` above) — this is the backstop for anything that still
       refuses to break, e.g. a `white-space: pre` notice added later. */
    @apply overflow-hidden;
  }
</style>
