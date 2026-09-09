<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import DialogShell from "./DialogShell.svelte";
  import { enterEscape } from "./shortcuts";
  import { tr } from "$lib/i18n";

  const dispatch = createEventDispatcher<{ confirm: void; cancel: void }>();

  interface Props {
    open?: boolean;
    title?: string;
    message?: string;
    confirmText?: string;
    danger?: boolean;
  }

  let {
    open = false,
    title = tr("common.confirm"),
    message = "",
    confirmText = tr("common.ok"),
    danger = false,
  }: Props = $props();

  function cancel() {
    open = false;
    dispatch("cancel");
  }

  function confirm() {
    open = false;
    dispatch("confirm");
  }
</script>

{#if open}
  <div
    class="fixed inset-0 z-[80] flex items-center justify-center"
    use:enterEscape={{ onEscape: cancel }}
  >
    <!-- Backdrop as a real (non-focusable) close button. -->
    <button
      type="button"
      class="absolute inset-0 cursor-default bg-black/40"
      style="border:0;padding:0"
      aria-label={tr("common.close")}
      tabindex="-1"
      onclick={cancel}>&#8203;</button
    >
    <DialogShell
      {title}
      {message}
      {confirmText}
      {danger}
      onConfirm={confirm}
      onCancel={cancel}
    />
  </div>
{/if}
