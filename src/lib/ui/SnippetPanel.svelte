<!-- @component 「命令片段」 sidebar: saved commands + the shell's history. -->
<script lang="ts">
  import { onMount } from "svelte";

  import {
    ClipboardListIcon,
    PlusIcon,
    RefreshCwIcon,
    ScrollTextIcon,
    SearchIcon,
    TextCursorInputIcon,
    TrashIcon,
    XIcon,
  } from "$lib/icons";
  import type { CommandSnippet, WsClient, WsServer } from "$lib/protocol";
  import {
    addSnippet,
    deleteSnippet,
    restoreSnippet,
    servers,
    updateSnippet,
  } from "$lib/connections";
  import { lang, t } from "$lib/i18n";
  import { matchesQuery, mergeHistory, sessionCommands } from "$lib/snippets";
  import type { Srocket } from "$lib/srocket";
  import { makeToast, toastError } from "$lib/toast";
  import { sidebarResize } from "./dragResize";
  import Sidebar from "./Sidebar.svelte";
  import SnippetDialog from "./SnippetDialog.svelte";

  interface Props {
    srocket: Srocket<WsServer, WsClient> | null;
    /** Active terminal sid (null when there is none: typing and history need it). */
    shellId: number | null;
    open?: boolean;
    connected?: boolean;
    /** Type `text` into the active terminal **without** a trailing Enter. The
     *  fold from multiple lines to one and the focus handling live in the
     *  parent (`Session.svelte`), which owns the terminals. */
    onTypeIn?: (text: string) => void;
  }

  let {
    srocket,
    shellId,
    open = true,
    connected = false,
    onTypeIn = () => {},
  }: Props = $props();

  type Mode = "snippets" | "history";

  /** Card chrome shared by **both** lists (saved snippets and history rows).
   *
   *  One constant on purpose: the two modes are the same list with a different
   *  data source, so their cards must be pixel-identical — a per-mode class
   *  string drifts the moment one side is touched (that is exactly how history
   *  ended up without the elevation). Elevation is `shadow-sm shadow-black/40`
   *  because the default `shadow-sm` is invisible on the panel's `bg-zinc-900`;
   *  hover is **only** `hover:bg-zinc-800`, which is the SFTP list's row hover
   *  (`FileManager.svelte`, `role="option"`), so both lists feel like one list. */
  const CARD_CLASS =
    "group rounded-md border border-zinc-800 bg-zinc-900 px-2.5 py-2 shadow-sm shadow-black/40 transition-colors hover:bg-zinc-800";

  let mode = $state<Mode>("snippets");
  let searchQuery = $state("");
  let query = $derived(searchQuery.trim().toLowerCase());

  /** Saved snippets (server-side, global — see `connections.ts`). */
  let snippets = $derived($servers.snippets);
  let visibleSnippets = $derived(
    snippets.filter((snippet) => matchesQuery(snippet.command, query)),
  );

  // ---- History ------------------------------------------------------------
  /** History entries from the shell's history file, per sid. Kept per sid so
   *  switching tabs back and forth does not re-read the file every time. */
  let fileHistory = $state<Record<number, string[]>>({});
  let historyLoading = $state(false);
  let historyNotice = $state<string | null>(null);
  /** The sid the in-flight request belongs to (a reply for another shell —
   *  the active tab changed while it was in flight — is dropped). */
  let historyPending = $state<number | null>(null);

  /** The merged list: this page's submitted commands in front of the file's.
   *  Reads the reactive store (not a `get()`) so a command typed into the
   *  terminal shows up in history mode immediately. */
  let historyRows = $derived.by(() => {
    const sid = shellId;
    if (sid === null) return [] as string[];
    return mergeHistory($sessionCommands[sid] ?? [], fileHistory[sid] ?? []);
  });
  let visibleHistory = $derived(
    historyRows.filter((row) => matchesQuery(row, query)),
  );

  function requestHistory() {
    const sid = shellId;
    if (!srocket || sid === null || !connected) return;
    historyPending = sid;
    historyLoading = true;
    historyNotice = null;
    srocket.send({ commandHistoryRequest: sid });
  }

  /** `commandHistory` reply from the server (routed by `Session.svelte`). */
  export function handleHistory(
    sid: number,
    entries: string[],
    notice: string | null,
  ) {
    if (historyPending !== sid) return; // a stale reply for a previous tab
    historyPending = null;
    historyLoading = false;
    fileHistory = { ...fileHistory, [sid]: entries };
    historyNotice = notice;
    if (notice) makeToast({ kind: "error", message: notice });
  }

  /** Entering history mode reads the file; leaving it does not touch anything
   *  (the entries stay cached for the next switch). The search box is cleared
   *  because a filter for one list rarely describes the other. */
  function toggleMode() {
    mode = mode === "snippets" ? "history" : "snippets";
    searchQuery = "";
    if (mode === "history") requestHistory();
  }

  /** Re-read on tab switch while showing history, so the list belongs to the
   *  terminal the user is looking at. Reads `shellId`, `mode`, `open` and the
   *  per-shell cache — the cache read is what stops it from re-requesting a
   *  file it already has. */
  $effect(() => {
    const sid = shellId;
    if (mode !== "history" || !open || sid === null) return;
    if (fileHistory[sid] !== undefined) return;
    requestHistory();
  });

  // ---- Add / edit / delete ------------------------------------------------
  let dialogOpen = $state(false);
  /** The snippet the dialog is editing; `null` = the 「新增命令」 flow. Both
   *  flows share one dialog (`SnippetDialog`), so the width/textarea/validation
   *  cannot drift apart. */
  let editing = $state<CommandSnippet | null>(null);

  function openAddDialog() {
    editing = null;
    dialogOpen = true;
  }

  /** Double-clicking a card edits it in the same dialog, pre-filled. */
  function openEditDialog(snippet: CommandSnippet) {
    editing = snippet;
    dialogOpen = true;
  }

  async function confirmDialog(command: string) {
    const target = editing;
    dialogOpen = false;
    editing = null;
    try {
      if (target) {
        await updateSnippet(target.id, command);
        makeToast({ kind: "success", message: t($lang, "snippet.updated") });
      } else {
        await addSnippet(command);
        makeToast({ kind: "success", message: t($lang, "snippet.added") });
      }
    } catch (err) {
      toastError(err);
    }
  }

  /** Add a history row to the saved snippets (history mode's 「添加」). */
  async function addFromHistory(command: string) {
    try {
      await addSnippet(command);
      makeToast({ kind: "success", message: t($lang, "snippet.added") });
    } catch (err) {
      toastError(err);
    }
  }

  /** Delete one card immediately (no confirmation).
   *
   *  The undo lives in the toast: the snippet and the index it occupied are
   *  captured before the request, so 「撤销」 puts the card back where it was
   *  rather than at the end of the list. `makeToast` de-duplicates identical
   *  (kind, message) toasts, so the command name is part of the text — two
   *  deletions in a row must each get their own toast and their own undo. */
  async function deleteCard(snippet: CommandSnippet) {
    const index = snippets.findIndex((s) => s.id === snippet.id);
    try {
      await deleteSnippet(snippet.id);
      makeToast(
        {
          kind: "info",
          message: t($lang, "snippet.deleted", {
            command: preview(snippet.command),
          }),
          action: t($lang, "snippet.undo"),
          onAction: () => {
            void restoreSnippet(snippet, index).catch((err) => toastError(err));
          },
        },
        // 10s: long enough to notice the card is gone and decide to undo it.
        10000,
      );
    } catch (err) {
      toastError(err);
    }
  }

  /** One-line, length-capped form of a command for toast text. */
  function preview(command: string): string {
    const single = command.replace(/\s+/g, " ").trim();
    return single.length > 40 ? `${single.slice(0, 40)}…` : single;
  }

  function typeInto(command: string) {
    if (shellId === null) {
      makeToast({ kind: "info", message: t($lang, "snippet.noTerminal") });
      return;
    }
    onTypeIn(command);
  }

  onMount(() => {
    // Nothing to load up-front: the saved snippets come from the store and the
    // history file is fetched when history mode is first entered.
  });

  function clearSearch() {
    searchQuery = "";
  }
</script>

<Sidebar resize={sidebarResize} {open} showHandle={open}>
  <!-- Search row: the panel has no title bar (the top-bar button is the title),
       so the search box is the first row. The count lives in the placeholder
       instead of a header counter. `Refresh` belongs here too: it re-reads the
       history file, i.e. it is a search-scope action, not a mode switch. -->
  <div class="flex h-8 items-center gap-1 border-b border-zinc-800 px-3">
    <SearchIcon size="13" class="shrink-0 text-zinc-500" />
    <input
      bind:value={searchQuery}
      class="min-w-0 flex-1 bg-transparent text-xs text-zinc-300 outline-none placeholder:text-zinc-600"
      placeholder={mode === "history"
        ? t($lang, "snippet.searchHistory", { n: historyRows.length })
        : t($lang, "snippet.search", { n: snippets.length })}
      onkeydown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          clearSearch();
        }
      }}
    />
    {#if searchQuery}
      <button
        class="shrink-0 text-zinc-500 transition-colors hover:text-zinc-200"
        onclick={clearSearch}
        title={t($lang, "snippet.clearSearch")}
      >
        <XIcon size="12" />
      </button>
    {/if}
    {#if mode === "history"}
      <button
        class="icon-btn-sm shrink-0"
        onclick={requestHistory}
        disabled={historyLoading || shellId === null || !connected}
        title={t($lang, "snippet.refreshHistory")}
        aria-label={t($lang, "snippet.refreshHistory")}
      >
        <RefreshCwIcon size="13" class={historyLoading ? "animate-spin" : ""} />
      </button>
    {/if}
  </div>

  <!-- Actions: new command + switch between saved snippets and history -->
  <div class="flex items-center gap-2 border-b border-zinc-800 px-3 py-2">
    <button
      class="flex min-w-0 flex-1 items-center justify-center gap-1 rounded-md bg-indigo-900 px-2 py-1.5 text-xs font-medium text-indigo-100 transition-colors hover:bg-indigo-800"
      onclick={openAddDialog}
      title={t($lang, "snippet.addHint")}
    >
      <PlusIcon size="13" class="shrink-0" />
      <span class="truncate">{t($lang, "snippet.add")}</span>
    </button>
    <button
      class="flex min-w-0 flex-1 items-center justify-center gap-1 rounded-md border border-zinc-700 px-2 py-1.5 text-xs text-zinc-300 transition-colors hover:bg-zinc-800 hover:text-zinc-100"
      onclick={toggleMode}
      title={mode === "history"
        ? t($lang, "snippet.backHint")
        : t($lang, "snippet.historyHint")}
    >
      {#if mode === "history"}
        <ScrollTextIcon size="13" class="shrink-0" />
      {:else}
        <ClipboardListIcon size="13" class="shrink-0" />
      {/if}
      <span class="truncate">
        {mode === "history"
          ? t($lang, "snippet.back")
          : t($lang, "snippet.history")}
      </span>
    </button>
  </div>

  <!-- List -->
  <div class="no-scrollbar flex-1 overflow-y-auto p-2">
    {#if mode === "snippets"}
      <div class="flex flex-col gap-2">
        {#each visibleSnippets as snippet (snippet.id)}
          <!-- A card, not a flat row — the chrome lives in `CARD_CLASS` so this
               list and the history list cannot drift apart. `svelte-ignore`
               mirrors `Servers.svelte`, which also puts a dblclick on a plain
               card div. -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class={CARD_CLASS}
            title={t($lang, "snippet.editHint")}
            ondblclick={(event) => {
              // A double-click on 键入/删除 is that button's business (the
              // second click is a no-op there); opening the editor for a card
              // whose delete just fired would be a lie.
              if (
                event.target instanceof Element &&
                event.target.closest("button")
              )
                return;
              openEditDialog(snippet);
            }}
          >
            <div class="flex items-start gap-1.5">
              <!-- The command wraps inside the sidebar; `whitespace-pre-wrap`
                   keeps the author's line breaks visible while `break-words`
                   (overflow-wrap) lets an unbroken long token wrap too. -->
              <p
                class="min-w-0 flex-1 whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-zinc-200"
              >
                {snippet.command}
              </p>
              <!-- Actions stack vertically (键入 on top, delete below): the row
                   of two icons next to a wrapped command reads as one block and
                   keeps the card's text column as wide as possible. -->
              <div class="flex shrink-0 flex-col items-center gap-0.5">
                <button
                  class="icon-btn-sm text-sky-400 hover:bg-sky-900/40"
                  onclick={() => typeInto(snippet.command)}
                  title={t($lang, "snippet.typeHint")}
                  aria-label={t($lang, "snippet.typeHint")}
                >
                  <TextCursorInputIcon size="14" />
                </button>
                <button
                  class="icon-btn-sm text-red-400 hover:bg-red-900/40"
                  onclick={() => void deleteCard(snippet)}
                  title={t($lang, "snippet.deleteHint")}
                  aria-label={t($lang, "snippet.deleteHint")}
                >
                  <TrashIcon size="14" />
                </button>
              </div>
            </div>
          </div>
        {:else}
          <p class="px-2 py-6 text-center text-xs text-zinc-500">
            {snippets.length === 0
              ? t($lang, "snippet.empty")
              : t($lang, "snippet.noMatch")}
          </p>
        {/each}
      </div>
    {:else}
      <div class="flex flex-col gap-2">
        {#if historyLoading || (shellId !== null && fileHistory[shellId] === undefined)}
          <p
            class="flex items-center justify-center gap-2 px-2 py-6 text-xs text-zinc-500"
          >
            <RefreshCwIcon size="12" class="animate-spin" />
            {t($lang, "snippet.historyLoading")}
          </p>
        {:else if visibleHistory.length === 0}
          <p class="px-2 py-6 text-center text-xs text-zinc-500">
            {historyRows.length === 0
              ? historyNotice
                ? t($lang, "snippet.historyFail")
                : t($lang, "snippet.historyEmpty")
              : t($lang, "snippet.noMatch")}
          </p>
        {:else}
          {#each visibleHistory as command (command)}
            <!-- Same card chrome as the saved snippets (`CARD_CLASS`): the two
                 modes are one list with a different data source, so the
                 elevation and the SFTP-style hover must not diverge. -->
            <div class={CARD_CLASS}>
              <div class="flex items-start gap-1.5">
                <p
                  class="min-w-0 flex-1 whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-zinc-200"
                >
                  {command}
                </p>
                <!-- History rows keep the same vertical button stack, with 键入 on
                   top and 添加 below (the same two-slot shape as a snippet card,
                   so the button columns line up across both modes). -->
                <div class="flex shrink-0 flex-col items-center gap-0.5">
                  <button
                    class="icon-btn-sm text-sky-400 hover:bg-sky-900/40"
                    onclick={() => typeInto(command)}
                    title={t($lang, "snippet.typeHint")}
                    aria-label={t($lang, "snippet.typeHint")}
                  >
                    <TextCursorInputIcon size="14" />
                  </button>
                  <button
                    class="icon-btn-sm text-amber-400 hover:bg-amber-900/40"
                    onclick={() => addFromHistory(command)}
                    title={t($lang, "snippet.addFromHistory")}
                    aria-label={t($lang, "snippet.addFromHistory")}
                  >
                    <PlusIcon size="14" />
                  </button>
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
</Sidebar>

<SnippetDialog
  open={dialogOpen}
  {editing}
  onConfirm={confirmDialog}
  onCancel={() => {
    dialogOpen = false;
    editing = null;
  }}
/>
