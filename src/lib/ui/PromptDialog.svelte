<script lang="ts">
  import { createEventDispatcher, tick } from "svelte";

  import DialogShell from "./DialogShell.svelte";
  import Dialog from "./Dialog.svelte";
  import { enterEscape } from "./shortcuts";
  import { tr } from "$lib/i18n";

  const dispatch = createEventDispatcher<{
    confirm: string;
    cancel: void;
  }>();

  export let open = false;
  export let title = tr("common.confirm");
  export let message = "";
  export let label = "";
  export let value = "";
  export let confirmText = tr("common.ok");
  export let placeholder = "";
  export let type: "text" | "password" = "text";

  let inputEl: HTMLInputElement;

  /** Focus the input as soon as the dialog opens (the previous headlessui
   *  FocusTrap is gone, so focus is set explicitly). */
  $: if (open) {
    tick().then(() => inputEl?.focus());
  }

  function cancel() {
    open = false;
    dispatch("cancel");
  }

  function confirm() {
    open = false;
    dispatch("confirm", value);
  }
</script>

<Dialog {open} onClose={cancel} z={90} panelClass="w-full max-w-sm">
  <DialogShell
    {title}
    {message}
    {confirmText}
    onConfirm={confirm}
    onCancel={cancel}
  >
    {#if label}
      <label for="prompt-input" class="mt-3 block text-sm text-zinc-300"
        >{label}</label
      >
    {/if}
    <input
      id="prompt-input"
      bind:this={inputEl}
      class="mt-2 w-full rounded-md border border-zinc-700 bg-zinc-800 px-3 py-2 text-sm text-zinc-100 outline-none focus:ring-2 focus:ring-indigo-500/50"
      {type}
      {value}
      on:input={(event) => (value = event.currentTarget.value)}
      {placeholder}
      use:enterEscape={{ onEnter: confirm, preventDefault: true }}
    />
  </DialogShell>
</Dialog>
