<script lang="ts">
  import { createEventDispatcher, tick } from "svelte";
  import {
    Dialog,
    DialogOverlay,
    Transition,
    TransitionChild,
  } from "@rgossiaux/svelte-headlessui";

  import DialogShell from "./DialogShell.svelte";
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

  /** Focus the input as soon as the dialog opens. The headlessui FocusTrap
   *  only auto-focuses when it is the *only* dialog open, so when this prompt
   *  is layered above another dialog (e.g. the server form) focus must be set
   *  explicitly. */
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

  /** headlessui's own Escape handling is disabled while another dialog is also
   *  open (`containers.size > 1`); keep Escape closing this prompt. */
  function windowKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && open) cancel();
  }
</script>

<svelte:window on:keydown={windowKeydown} />

<Transition show={open}>
  <Dialog
    on:close={cancel}
    class="fixed inset-0 z-[90] grid place-items-center"
  >
    <DialogOverlay class="fixed -z-10 inset-0 bg-black/40" />

    <TransitionChild
      enter="duration-300 ease-out"
      enterFrom="scale-95 opacity-0"
      enterTo="scale-100 opacity-100"
      leave="duration-75 ease-out"
      leaveFrom="scale-100 opacity-100"
      leaveTo="scale-95 opacity-0"
      class="w-full max-w-sm"
    >
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
    </TransitionChild>
  </Dialog>
</Transition>
