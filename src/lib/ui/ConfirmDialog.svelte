<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import DialogShell from "./DialogShell.svelte";
  import { enterEscape } from "./shortcuts";
  import { noop } from "./a11y";
  import { tr } from "$lib/i18n";

  const dispatch = createEventDispatcher<{ confirm: void; cancel: void }>();

  export let open = false;
  export let title = tr("common.confirm");
  export let message = "";
  export let confirmText = tr("common.ok");
  export let danger = false;

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
    <div
      class="absolute inset-0 bg-black/40"
      on:click={cancel}
      on:keydown={noop}
      use:enterEscape={{ onEscape: cancel }}
    />
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
