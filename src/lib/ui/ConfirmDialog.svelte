<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import Dialog from "./Dialog.svelte";
  import DialogShell from "./DialogShell.svelte";
  import { tr } from "$lib/i18n";

  const dispatch = createEventDispatcher<{
    confirm: void;
    cancel: void;
    /** Optional secondary action (e.g. "跳过" on overwrite prompts). */
    middle: void;
  }>();

  interface Props {
    open?: boolean;
    title?: string;
    message?: string;
    confirmText?: string;
    /** Label for the optional secondary (middle) action, between cancel and confirm. */
    middleText?: string;
    danger?: boolean;
  }

  let {
    open = false,
    title = tr("common.confirm"),
    message = "",
    confirmText = tr("common.ok"),
    middleText = "",
    danger = false,
  }: Props = $props();

  function cancel() {
    open = false;
    dispatch("cancel");
  }

  function middle() {
    open = false;
    dispatch("middle");
  }

  function confirm() {
    open = false;
    dispatch("confirm");
  }
</script>

<Dialog {open} onClose={cancel} z={80} panelClass="w-full max-w-sm">
  <DialogShell
    {title}
    {message}
    {confirmText}
    {danger}
    {middleText}
    onConfirm={confirm}
    onMiddle={middle}
    onCancel={cancel}
  />
</Dialog>
