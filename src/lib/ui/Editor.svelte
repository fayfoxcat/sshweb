<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import {
    CheckIcon,
    ChevronDownIcon,
    CornerUpLeftIcon,
    CornerUpRightIcon,
    MinusIcon,
    RotateCcwIcon,
    SaveIcon,
    SearchIcon,
    XIcon,
  } from "svelte-feather-icons";

  import type { WsClient, WsServer } from "$lib/protocol";
  import { lang, t } from "$lib/i18n";
  import { makeToast } from "$lib/toast";
  import type { Srocket } from "$lib/srocket";
  import {
    decodeBytes,
    encodeText,
    ENCODINGS,
    type Encoding,
  } from "$lib/encoding";
  import { formatSize } from "$lib/format";
  import { basename } from "$lib/path";
  import {
    EDITOR_DRAFT_MAX_BYTES,
    EDITOR_DRAFT_TTL_MS,
    EDITOR_DRAFTS_KEY,
  } from "$lib/constants";
  import { storageGet, storageSet } from "$lib/storage";
  import { editorKey } from "$lib/session/editors";
  import CodeEditor from "./CodeEditor.svelte";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import { createPointerDrag } from "./dragResize";
  import { LIGHT_MODE_LINE_LIMIT } from "$lib/editor/languages";

  export let srocket: Srocket<WsServer, WsClient> | null;
  export let shellId: number | null;
  export let filePath: string;
  export let active = true;
  /** Stacking order: higher = on top (multi-window layering). */
  export let zIndex = 60;
  export let onClose: () => void = () => {};
  export let onMinimize: () => void = () => {};
  /** Called when the user clicks this window (bring-to-front). */
  export let onActivate: () => void = () => {};
  /** Called when the edited/dirty state changes (for the tab indicator). */
  export let onEditedChange: (dirty: boolean) => void = () => {};
  /** Called to re-read the file from disk (the 还原 button reloads the latest
   *  server content instead of reverting to an in-memory baseline). */
  export let onReload: () => void = () => {};

  // Encoding choices.

  let encoding: Encoding = "utf-8";
  let encodingMenu = false;

  let edited = false;
  let lightMode = false;
  let cursorLine = 1;
  let cursorCol = 1;
  let editor: CodeEditor;
  /** Whether the search/replace panel is open (drives the toolbar button). */
  let searchOpen = false;

  // Window position / size (centered at 80% initially, then draggable).
  let winW = Math.round(window.innerWidth * 0.8);
  let winH = Math.round(window.innerHeight * 0.8);
  let posX = 0;
  let posY = 0;
  let dragStartMX = 0;
  let dragStartMY = 0;
  let dragStartX = 0;
  let dragStartY = 0;
  let resizeStartMX = 0;
  let resizeStartMY = 0;
  let resizeStartW = 0;
  let resizeStartH = 0;

  /** Window drag (title bar) and resize share one pointer-capture state
   *  machine (`createPointerDrag`), each with its own active gate. */
  const winDrag = createPointerDrag({
    start(event: PointerEvent) {
      if ((event.target as HTMLElement).closest("button, input, select")) {
        return false;
      }
      dragStartMX = event.clientX;
      dragStartMY = event.clientY;
      dragStartX = posX;
      dragStartY = posY;
      onActivate();
      event.preventDefault();
      return true;
    },
    move(event: PointerEvent) {
      posX = dragStartX + (event.clientX - dragStartMX);
      posY = dragStartY + (event.clientY - dragStartMY);
    },
    end() {},
  });

  const resizeDrag = createPointerDrag({
    start(event: PointerEvent) {
      resizeStartMX = event.clientX;
      resizeStartMY = event.clientY;
      resizeStartW = winW;
      resizeStartH = winH;
      event.preventDefault();
      event.stopPropagation();
      return true;
    },
    move(event: PointerEvent) {
      winW = Math.max(320, resizeStartW + (event.clientX - resizeStartMX));
      winH = Math.max(200, resizeStartH + (event.clientY - resizeStartMY));
    },
    end() {},
  });

  let pendingBytes: Uint8Array | null = null;

  // ---- Unsaved-draft persistence (survives a browser refresh) -----------
  type Draft = { text: string; encoding: Encoding; savedAt: number };
  /** Composite draft key: `sid:path` — the same path on different servers is
   *  a different file, so drafts must not collide across servers. */
  function draftKey(): string {
    return editorKey(shellId ?? -1, filePath);
  }
  function readDrafts(): Record<string, Draft> {
    return storageGet<Record<string, Draft>>(EDITOR_DRAFTS_KEY, {}, (raw) => {
      const all = JSON.parse(raw) as Record<string, Draft>;
      const now = Date.now();
      const fresh: Record<string, Draft> = {};
      let dropped = false;
      for (const [p, d] of Object.entries(all)) {
        // Drop drafts that haven't been touched for the TTL (they could be
        // stale against a file that changed on disk). Legacy drafts saved
        // before the TTL existed (no `savedAt`) are kept — they are still
        // valid unsaved content.
        const stale =
          d &&
          typeof d.savedAt === "number" &&
          now - d.savedAt > EDITOR_DRAFT_TTL_MS;
        if (d && !stale) {
          fresh[p] = d;
        } else {
          dropped = true;
        }
      }
      if (dropped) writeDrafts(fresh);
      return fresh;
    });
  }
  function writeDrafts(drafts: Record<string, Draft>) {
    storageSet(EDITOR_DRAFTS_KEY, drafts);
  }
  function saveDraft() {
    const text = editor?.getDoc() ?? "";
    if (text.length > EDITOR_DRAFT_MAX_BYTES) return; // skip huge files
    const drafts = readDrafts();
    drafts[draftKey()] = { text, encoding, savedAt: Date.now() };
    writeDrafts(drafts);
  }
  function clearDraft() {
    if (draftTimer) {
      clearTimeout(draftTimer);
      draftTimer = null;
    }
    const drafts = readDrafts();
    if (drafts[draftKey()]) {
      delete drafts[draftKey()];
      writeDrafts(drafts);
    }
  }
  let draftTimer: ReturnType<typeof setTimeout> | null = null;
  /** True while `loadContent` runs: loading disk content (via `setDoc`)
   *  reports `dirty=false` through the CodeMirror listener, which must NOT
   *  clear the unsaved draft — the draft is overlaid right after by
   *  `restoreDraft`. Only a real user undo-to-clean clears the draft. */
  let suppressingClear = false;

  // Flush a pending debounced draft on refresh/unload so the latest keystroke
  // survives.
  onMount(() => {
    window.addEventListener("beforeunload", saveDraft);
    return () => window.removeEventListener("beforeunload", saveDraft);
  });
  onDestroy(() => {
    if (draftTimer) {
      clearTimeout(draftTimer);
      draftTimer = null;
    }
  });

  // ---- Load content ------------------------------------------------------
  function loadContent(data: Uint8Array) {
    pendingBytes = data;
    const text = decodeBytes(data, encoding);
    lightMode = text.split("\n").length > LIGHT_MODE_LINE_LIMIT;
    // Suppress draft-clear while setting the on-disk content; the draft is
    // overlaid right after (see `suppressingClear`).
    suppressingClear = true;
    setEdited(false);
    editor?.setDoc(text);
    restoreDraft();
    suppressingClear = false;
  }

  /** Overlay an unsaved draft (if any) on freshly-loaded disk content, keeping
   *  the on-disk content as the diff baseline. Restores the draft's encoding
   *  too, so a later save re-encodes with the right one. */
  function restoreDraft() {
    const drafts = readDrafts();
    // Legacy fallback: drafts saved before per-server keying used the bare
    // path; migrate them to `sid:path` on restore.
    let draft = drafts[draftKey()];
    if (!draft && drafts[filePath]) {
      draft = drafts[filePath];
      delete drafts[filePath];
      drafts[draftKey()] = draft;
      writeDrafts(drafts);
    }
    if (!draft) return;
    if (draft.encoding !== encoding) encoding = draft.encoding;
    editor?.replaceDoc(draft.text);
    setEdited(true);
  }

  function handleEditedChange(dirty: boolean) {
    setEdited(dirty);
    if (dirty) {
      // Debounce so keystrokes don't hammer sessionStorage.
      if (draftTimer) clearTimeout(draftTimer);
      draftTimer = setTimeout(saveDraft, 400);
    } else if (!suppressingClear) {
      clearDraft();
    }
  }

  function setEdited(value: boolean) {
    if (edited !== value) {
      edited = value;
      onEditedChange(value);
    }
  }

  // ---- Save / revert / close --------------------------------------------
  let confirmOpen = false;
  let confirmTitle = "";
  let confirmMessage = "";
  let confirmDanger = false;
  let confirmAction: "revert" | "close" | "encoding" | null = null;
  let pendingEncoding: Encoding = "utf-8";

  function askConfirm(
    title: string,
    message: string,
    action: "revert" | "close" | "encoding",
    danger = false,
    enc?: Encoding,
  ) {
    confirmTitle = title;
    confirmMessage = message;
    confirmAction = action;
    confirmDanger = danger;
    if (enc) pendingEncoding = enc;
    confirmOpen = true;
  }

  function onConfirmResult() {
    switch (confirmAction) {
      case "revert":
        doRevert();
        break;
      case "close":
        onClose();
        break;
      case "encoding":
        doChangeEncoding(pendingEncoding);
        break;
    }
    confirmAction = null;
    confirmOpen = false;
  }

  function save() {
    if (!srocket || shellId === null) return;
    if (!edited) return;
    const text = editor?.getDoc() ?? "";
    const bytes = encodeText(text, encoding);
    savePending = true;
    saveText = text;
    srocket.send({ sftpWrite: [shellId, filePath, bytes] });
  }

  function revert() {
    if (edited) {
      askConfirm(
        t($lang, "editor.revertTitle"),
        t($lang, "editor.revertMessage"),
        "revert",
      );
    } else {
      doRevert();
    }
  }

  /** Discard unsaved edits and re-read the file from disk: the file may have
   *  changed on the server since it was opened, so the "还原" button reloads
   *  instead of reverting to the in-memory snapshot. The draft is cleared
   *  first so the incoming `loadContent` (via `sftpData`) doesn't overlay it.
   */
  function doRevert() {
    clearDraft();
    // No save is in flight any more — a late `sftpOk` must not reset the
    // diff baseline to the pre-reload text.
    savePending = false;
    saveText = "";
    setEdited(false);
    onReload();
    makeToast({ kind: "info", message: t($lang, "editor.reloaded") });
  }

  function close() {
    if (edited) {
      askConfirm(
        t($lang, "editor.closeDirtyTitle"),
        t($lang, "editor.closeDirtyMessage"),
        "close",
        true,
      );
    } else {
      onClose();
    }
  }

  /** Called when this editor's tab is closed: discard any unsaved draft. */
  export function markClosed() {
    clearDraft();
  }

  // ---- Undo / redo ------------------------------------------------------
  function undo() {
    editor?.undo();
  }

  function redo() {
    editor?.redo();
  }

  // ---- Encoding change ---------------------------------------------------
  function changeEncoding(enc: Encoding) {
    encodingMenu = false;
    if (enc === encoding) return;
    if (edited) {
      askConfirm(
        t($lang, "editor.encodingTitle"),
        t($lang, "editor.encodingMessage"),
        "encoding",
        false,
        enc,
      );
      return;
    }
    doChangeEncoding(enc);
  }

  function doChangeEncoding(enc: Encoding) {
    encoding = enc;
    if (pendingBytes) loadContent(pendingBytes);
  }

  // ---- Public API --------------------------------------------------------
  export function loadFile(data: Uint8Array) {
    loadContent(data);
  }

  /** Reset the diff baseline and clear the "modified" indicator when the
   *  server acknowledges a save (`sftpOk`). Only acts when a save is actually
   *  in flight for this editor: an unrelated `sftpOk` for the path must not
   *  reset the baseline (which would silently discard unsaved edits). */
  let savePending = false;
  /** The document text captured when the pending save was sent; the on-disk
   *  content becomes this, so edits made while the save was in flight stay
   *  marked as modified. */
  let saveText = "";
  export function markSaved() {
    if (!savePending) return;
    savePending = false;
    const text = editor?.getDoc() ?? "";
    // The baseline is the content that was actually written to disk.
    editor?.setBaseline(saveText);
    // Edits made after the save was sent remain modified.
    setEdited(text !== saveText);
    if (text === saveText) clearDraft();
    makeToast({ kind: "success", message: t($lang, "editor.saved") });
  }

  function fileName(): string {
    return basename(filePath);
  }

  /** File size in human form; `-` until the content bytes have arrived. A
   *  reactive block (not a plain function read in the template) so the size
   *  updates when `pendingBytes` changes — a function call in the template
   *  only re-evaluates for variables referenced directly in the expression. */
  $: fileSizeText = pendingBytes ? formatSize(pendingBytes.length) : "-";
</script>

<div
  class="pointer-events-none fixed left-0 right-0 flex items-center justify-center"
  style:top="42px"
  style:bottom="32px"
  style:z-index={zIndex}
  style:display={active ? undefined : "none"}
>
  <div
    class="pointer-events-auto relative flex flex-col overflow-hidden rounded-lg border border-zinc-700 bg-[#111] text-zinc-200 shadow-2xl"
    style:width={`${winW}px`}
    style:height={`${winH}px`}
    style:transform={`translate(${posX}px, ${posY}px)`}
  >
    <!-- Title bar (drag handle) -->
    <div
      class="flex items-center gap-1.5 border-b border-zinc-800 bg-zinc-900 px-3 py-1.5"
      on:pointerdown={winDrag.onStart}
      on:pointermove={winDrag.onMove}
      on:pointerup={winDrag.onEnd}
      on:pointercancel={winDrag.onEnd}
    >
      <button
        class="icon-btn"
        on:click={close}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.close")}
      >
        <XIcon size="16" />
      </button>
      <button
        class="icon-btn"
        on:click={onMinimize}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.minimize")}
      >
        <MinusIcon size="16" />
      </button>
      <span class="max-w-[240px] truncate font-medium">{fileName()}</span>

      {#if edited}
        <span class="flex items-center gap-1 text-xs text-amber-400">
          <span class="h-1.5 w-1.5 rounded-full bg-amber-400" />{t(
            $lang,
            "editor.modified",
          )}
        </span>
      {/if}

      {#if lightMode}
        <span
          class="rounded bg-amber-900/40 px-1.5 py-0.5 text-[10px] text-amber-300"
          title={t($lang, "editor.lightTitle")}>{t($lang, "editor.light")}</span
        >
      {/if}

      <div class="flex-1" />

      <!-- Encoding selector -->
      <div class="relative">
        <button
          class="icon-btn flex items-center gap-1"
          on:click={() => (encodingMenu = !encodingMenu)}
          on:pointerdown={(e) => e.stopPropagation()}
          title={t($lang, "editor.encoding")}
        >
          <span class="text-xs uppercase">{encoding}</span>
          <ChevronDownIcon size="12" />
        </button>
        {#if encodingMenu}
          <div
            class="absolute right-0 top-full z-10 mt-1 w-36 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-lg"
          >
            {#each ENCODINGS as enc}
              <button
                class="flex w-full items-center justify-between px-3 py-1 text-left text-xs hover:bg-zinc-800"
                on:click={() => changeEncoding(enc)}
              >
                <span class="uppercase">{enc}</span>
                {#if encoding === enc}
                  <CheckIcon size="12" class="text-indigo-400" />
                {/if}
              </button>
            {/each}
          </div>
        {/if}
      </div>

      <button
        class="icon-btn"
        on:click={undo}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.undo")}
      >
        <CornerUpLeftIcon size="16" />
      </button>
      <button
        class="icon-btn"
        on:click={redo}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.redo")}
      >
        <CornerUpRightIcon size="16" />
      </button>
      <button
        class="icon-btn"
        on:click={revert}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.revert")}
      >
        <RotateCcwIcon size="16" />
      </button>
      <button
        class="icon-btn"
        class:bg-zinc-700={searchOpen}
        class:text-zinc-100={searchOpen}
        on:click={() => editor?.toggleSearch()}
        on:pointerdown={(e) => e.stopPropagation()}
        title={t($lang, "editor.search")}
      >
        <SearchIcon size="16" />
      </button>
      <button
        class="icon-btn flex items-center gap-1 text-emerald-400 hover:bg-emerald-900/40 disabled:opacity-40"
        on:click={save}
        on:pointerdown={(e) => e.stopPropagation()}
        disabled={!edited}
        title={t($lang, "editor.save")}
      >
        <SaveIcon size="16" />
        <span class="text-xs">{t($lang, "editor.save")}</span>
      </button>
    </div>

    <!-- Editor -->
    <div class="min-h-0 flex-1 overflow-hidden">
      <CodeEditor
        bind:this={editor}
        {filePath}
        {lightMode}
        onEditedChange={handleEditedChange}
        onCursorChange={(line, col) => {
          cursorLine = line;
          cursorCol = col;
        }}
        onSearchOpenChange={(open) => (searchOpen = open)}
      />
    </div>

    <!-- Status bar -->
    <div
      class="flex items-center gap-4 border-t border-zinc-800 bg-zinc-900 px-3 py-1 text-xs text-zinc-400"
    >
      <span class="max-w-[520px] truncate">{filePath}</span>
      {#if lightMode}
        <span class="text-amber-400">{t($lang, "editor.lightStatus")}</span>
      {/if}
      <div class="flex-1" />
      <span
        >{t($lang, "editor.lineCol", {
          line: cursorLine,
          col: cursorCol,
        })}</span
      >
      <span class="uppercase">{encoding}</span>
      <span>{fileSizeText}</span>
    </div>

    <!-- Resize handle -->
    <div
      class="absolute bottom-0 right-0 h-5 w-5 cursor-nwse-resize"
      on:pointerdown={resizeDrag.onStart}
      on:pointermove={resizeDrag.onMove}
      on:pointerup={resizeDrag.onEnd}
      on:pointercancel={resizeDrag.onEnd}
    />
  </div>
</div>

<ConfirmDialog
  open={confirmOpen}
  title={confirmTitle}
  message={confirmMessage}
  danger={confirmDanger}
  on:confirm={onConfirmResult}
  on:cancel={() => {
    confirmAction = null;
    confirmOpen = false;
  }}
/>
