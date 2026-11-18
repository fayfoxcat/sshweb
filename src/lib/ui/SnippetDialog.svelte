<!-- @component Add / edit-command dialog for the 「命令片段」 sidebar. -->
<script lang="ts">
  import { MAX_SNIPPET_CHARS } from "$lib/constants";
  import { tr } from "$lib/i18n";

  import Dialog from "./Dialog.svelte";
  import DialogShell from "./DialogShell.svelte";

  interface Props {
    open?: boolean;
    /** The snippet being edited. `null` = the 「新增命令」 flow. Both flows share
     *  this dialog (same width, same textarea, same validation) — only the copy
     *  and the seeded draft differ. */
    editing?: { id: string; command: string } | null;
    /** Receives the trimmed command text; the dialog closes itself and the
     *  caller persists the snippet (add or update). */
    onConfirm: (command: string) => void;
    onCancel: () => void;
  }

  let { open = false, editing = null, onConfirm, onCancel }: Props = $props();

  let value = $state("");
  let error = $state("");

  /** Seed the draft each time the dialog opens: empty for a new command, the
   *  existing text when editing. Re-running only on the open edge also means a
   *  cancelled edit is not silently resurrected next time. */
  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen) {
      wasOpen = true;
      value = editing?.command ?? "";
      error = "";
    } else if (!open) {
      wasOpen = false;
    }
  });

  let title = $derived(
    editing ? tr("snippet.editTitle") : tr("snippet.dialogTitle"),
  );
  let message = $derived(
    editing ? tr("snippet.editMessage") : tr("snippet.dialogMessage"),
  );

  /** Validate in one place; the confirm button, Ctrl+Enter and the native form
   *  submit all funnel through here so they cannot disagree. */
  function submit() {
    const command = value.trim();
    if (!command) {
      error = tr("snippet.dialogEmpty");
      return;
    }
    if (command.length > MAX_SNIPPET_CHARS) {
      error = tr("snippet.dialogTooLong", { n: MAX_SNIPPET_CHARS });
      return;
    }
    error = "";
    onConfirm(command);
  }
</script>

<Dialog {open} onClose={onCancel} z={90} panelClass="w-full max-w-5xl">
  <DialogShell
    {title}
    {message}
    confirmText={tr("snippet.dialogConfirm")}
    widthClass="max-w-5xl"
    onConfirm={submit}
    {onCancel}
  >
    <form
      onsubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <!-- A textarea (not an input): commands can span several lines, and the
           sidebar renders them wrapped. Plain Enter is a line break; Ctrl/Cmd+
           Enter submits. -->
      <textarea
        class="mt-3 h-40 w-full resize-y rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 font-mono text-xs text-zinc-100 outline-none focus:ring-2 focus:ring-indigo-500/50"
        placeholder={tr("snippet.dialogPlaceholder")}
        spellcheck="false"
        autocomplete="off"
        bind:value
        oninput={() => (error = "")}
        onkeydown={(event) => {
          if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
            event.preventDefault();
            submit();
          }
        }}
      ></textarea>
      {#if error}
        <p class="mt-2 text-xs text-red-400">{error}</p>
      {:else}
        <p class="mt-2 text-xs text-zinc-500">{tr("snippet.dialogHint")}</p>
      {/if}
    </form>
  </DialogShell>
</Dialog>
