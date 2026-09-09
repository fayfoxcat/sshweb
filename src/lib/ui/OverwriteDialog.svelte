<script lang="ts">
  import { createEventDispatcher } from "svelte";

  import DialogShell from "./DialogShell.svelte";
  import { enterEscape } from "./shortcuts";
  import { lang, t } from "$lib/i18n";

  const dispatch = createEventDispatcher<{
    overwrite: void;
    skip: void;
    cancel: void;
  }>();

  export let open = false;
  export let names: string[] = [];
  export let title = "";
  /** Preview of the conflicting names (kept inside the component so the i18n
   *  separator / overflow suffix stay in one place). */
  $: preview =
    names.length <= 4
      ? names.join(t($lang, "file.zipSeparator"))
      : `${names.slice(0, 4).join(t($lang, "file.zipSeparator"))}…`;
  $: message = t($lang, "file.overwriteMessage", {
    n: names.length,
    names: preview,
  });

  function overwrite() {
    open = false;
    dispatch("overwrite");
  }

  function skip() {
    open = false;
    dispatch("skip");
  }

  function cancel() {
    open = false;
    dispatch("cancel");
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
      aria-label={t($lang, "common.close")}
      tabindex="-1"
      on:click={cancel}>&#8203;</button
    >
    <DialogShell
      {title}
      {message}
      confirmText={t($lang, "file.overwriteBtn")}
      middleText={t($lang, "file.skipBtn")}
      onConfirm={overwrite}
      onMiddle={skip}
      onCancel={cancel}
    />
  </div>
{/if}
