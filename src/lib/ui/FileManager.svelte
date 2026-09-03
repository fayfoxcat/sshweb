<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import {
    FolderIcon,
    RefreshCwIcon,
    SearchIcon,
    XIcon,
  } from "svelte-feather-icons";

  import type { WsClient, WsServer, WsSftpEntry } from "$lib/protocol";
  import { makeToast } from "$lib/toast";
  import { lang, t } from "$lib/i18n";
  import type { Srocket } from "$lib/srocket";
  import { formatMode, formatSize } from "$lib/format";
  import { joinPath, parentOf, basename } from "$lib/path";
  import { fileType } from "$lib/fileicons";
  import {
    collectDropFiles,
    onUploadAck,
    onUploadError,
    readDropPayload,
    startUpload,
    type DropPayload,
  } from "$lib/upload";
  import {
    archiveNameFor,
    sftpHttpPath,
    triggerBrowserDownload,
  } from "$lib/file/download";
  import { copyText } from "$lib/clipboard";
  import { MAX_FILE_ROWS, TOOLTIP_DELAY_MS } from "$lib/constants";
  import { writeSftpView } from "$lib/sftpView";
  import { sidebarResize } from "./dragResize";
  import { draggable, droppable } from "./dnd";
  import { enterEscape } from "./shortcuts";
  import { noop } from "./a11y";
  import ConfirmDialog from "./ConfirmDialog.svelte";
  import PromptDialog from "./PromptDialog.svelte";
  import OverwriteDialog from "./OverwriteDialog.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import UploadPanel from "./UploadPanel.svelte";
  import FileTooltip from "./FileTooltip.svelte";
  import Sidebar from "./Sidebar.svelte";

  const dispatch = createEventDispatcher<{
    sshInDir: { dir: string; sid: number | null };
    openEditor: { path: string; sid: number };
    /** The active terminal switched to a server never opened in the file
     *  manager; the parent opens that terminal's server (follow the active). */
    followActive: void;
  }>();

  const join = joinPath;

  export let srocket: Srocket<WsServer, WsClient> | null;
  /** The active shell id (drives which server the view follows). */
  export let shellId: number | null;
  /** Server identity per shell (`user@host:port` or "local"): the file-manager
   *  view is bound to a server (all its terminals share one SFTP view), so
   *  switching terminals of the same server keeps the view. */
  export let shellServerKeys: Record<number, string> = {};
  /** Server session key, used to build HTTP download URLs. */
  export let sessionName = "";
  /** Human-readable label for each target sid (open shells and headless SFTP
   *  shells), shown in the search hint instead of a selector. */
  export let targetNames: Record<number, string> = {};
  /** Whether the sidebar is visible. Kept mounted while hidden so transfers
   *  and messages continue to be handled. */
  export let open = true;
  /** False while the reconnect/initial replay is still settling: the view
   *  must not auto-follow (open never-opened servers) until the restored view
   *  has been applied. */
  export let replaySettled = false;

  // ---- State -------------------------------------------------------------
  let path = "/";
  let entries: WsSftpEntry[] = [];
  /** True when the last listing was capped by the server (too many entries). */
  let listTruncated = false;
  let loading = false;
  /** Back-stack of previously viewed directories (driven by the mouse
   *  side-buttons; pushed when navigating into a directory). */
  let history: string[] = [];
  /** Forward-stack, refilled when navigating back. */
  let forward: string[] = [];

  /** Search filter for the current directory listing. */
  let searchQuery = "";
  $: query = searchQuery.trim().toLowerCase();
  $: filtered = query
    ? entries.filter((e) => e.name.toLowerCase().includes(query))
    : entries;
  $: visibleEntries = filtered.slice(0, MAX_FILE_ROWS);

  /** Path bar editing. */
  let editingPath = false;
  let pathDraft = "/";
  let pathInput: HTMLInputElement;

  /** Rename-in-place target (entry name) or null. */
  let renamingName: string | null = null;
  let renameValue = "";
  let renameInput: HTMLInputElement;

  /** Hidden pickers for uploading files / folders from the context menu. */
  let fileInput: HTMLInputElement;
  let folderInput: HTMLInputElement;

  /** Multi-selection: set of entry names selected. */
  let selected: Set<string> = new Set();
  /** Last clicked/anchored entry name (for shift-range selection). */
  let anchorName: string | null = null;

  /** Hover tooltip state. */
  let hoverEntry: WsSftpEntry | null = null;
  let hoverX = 0;
  let hoverY = 0;

  /** Names selected for deletion (drives the ConfirmDialog). */
  let deleteTarget: string[] | null = null;
  /** Full paths of in-flight deletions, used to report success once all acks
   *  arrive (see `applyAck`). Cleared on the first failure. */
  let pendingDeletes: Set<string> = new Set();
  /** Entry names of in-flight deletions, shown in the success toast once all
   *  acks arrive (see `applyAck`). */
  let pendingDeleteNames: string[] = [];
  /** Name-input dialog state for "new file" / "new directory". */
  let promptDialog: { kind: "file" | "dir"; title: string } | null = null;
  let promptValue = "";

  /** Sort: directories first, then by name. */
  function sortEntries(list: WsSftpEntry[]): WsSftpEntry[] {
    return [...list].sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
  }

  /** The shell currently being browsed. Defaults to the active terminal's
   *  server; on terminal switches it only moves to a server that was already
   *  opened (restoring its directory). */
  let viewShellId: number | null = null;
  /** Per-server view state (browse sid + current directory), kept for the
   *  session: switching between servers restores each one's last directory. */
  let views = new Map<string, { sid: number; path: string }>();
  /** The server identity currently browsed (`user@host:port` or "local"). */
  let currentKey: string | null = null;
  /** Server identity of the active terminal, derived from the `shellId` prop
   *  (null while no terminal is active). */
  $: activeKey = shellId != null ? shellServerKeys[shellId] ?? null : null;
  /** Last observed active-server identity (the follow block writes it). */
  let prevActiveKey: string | null = null;
  /** Whether the file-manager panel was open on the previous reactive pass;
   *  the follow block re-evaluates when the panel *opens* — a terminal that
   *  became active while the panel was closed is followed when the sidebar is
   *  opened (otherwise the view stays unbound and refresh is a no-op). */
  let prevFollowOpen = false;

  /** Save the current view (browse sid + directory) under its server key. */
  function saveView() {
    if (currentKey !== null && viewShellId !== null) {
      views.set(currentKey, { sid: viewShellId, path });
    }
  }

  /** Resolve a pending paste conflict-check listing with `entries` (defaults to
   *  empty when the check failed or the target changed, so the paste proceeds
   *  without the overwrite prompt). */
  function resolvePendingList(entries: WsSftpEntry[] = []) {
    if (pendingList) {
      pendingList.resolve(entries);
      pendingList = null;
    }
  }

  /** Apply a browse target: bind the view to `key`/`sid` and navigate to
   *  `targetPath`, clearing per-directory transient state. The saved-views map
   *  is managed by the callers (and the navigation reactive below). */
  function applyBrowse(key: string, sid: number, targetPath: string) {
    const serverChanged = currentKey !== key;
    viewShellId = sid;
    currentKey = key;
    path = targetPath;
    if (serverChanged) {
      // Never show another server's stale listing while the new one loads.
      history = [];
      entries = [];
      listTruncated = false;
    }
    clearViewState();
    resolvePendingList();
    refresh();
  }

  /** Follow the active terminal's server: switching to a server with a saved
   *  view restores its directory; a server never opened in the file manager is
   *  opened following the active terminal (only while the panel is open and
   *  the replay has settled — never during a refresh's initial restore). */
  $: if (activeKey !== prevActiveKey || (open && !prevFollowOpen)) {
    prevActiveKey = activeKey;
    prevFollowOpen = open;
    if (
      open &&
      replaySettled &&
      activeKey !== null &&
      activeKey !== currentKey
    ) {
      const saved = views.get(activeKey);
      if (saved) {
        saveView();
        applyBrowse(activeKey, saved.sid, saved.path);
      } else {
        // Never opened this server: ask the parent to open the active
        // terminal's server (first open follows its known directory).
        saveView();
        dispatch("followActive");
      }
    }
  }

  /** Keep the current server's saved directory in sync with navigation. */
  $: if (viewShellId != null) saveView();

  /** Local terminals share one "local" view. Pin the browse sid to the
   *  currently-active local terminal so a closed terminal's sid can't leave
   *  the view stale — e.g. browse local → close that terminal → create a new
   *  local terminal: the server key is still "local", so the follow block
   *  above wouldn't re-open, but `viewShellId` still points at the closed
   *  terminal and every listing fails. Refreshing with the new active local
   *  sid keeps the shared directory while rebinding to a live terminal. */
  $: if (
    activeKey === currentKey &&
    activeKey === "local" &&
    shellId !== null &&
    viewShellId !== null &&
    viewShellId !== shellId &&
    shellServerKeys[shellId] === "local"
  ) {
    viewShellId = shellId;
    refresh();
  }

  /** Clear the per-directory transient state (forward stack, selection and
   *  search); the browsing path itself is handled by the caller. */
  function clearViewState() {
    forward = [];
    selected = new Set();
    anchorName = null;
    searchQuery = "";
  }

  /** Browse a server's file system (called by the parent once an
   *  `sftpOpenResult` resolves, keyed by server). `initialPath` seeds the view
   *  on the FIRST open of that server (e.g. the terminal's known directory);
   *  subsequent opens keep the server's saved directory. `explicit` is true
   *  only for server-list opens (a deliberate server target); a terminal or
   *  follow open with a stale result (the active terminal has since moved to
   *  another server) is ignored. */
  export function browseShell(
    sid: number,
    initialPath: string,
    key: string,
    explicit = true,
  ) {
    if (!explicit && key !== activeKey) {
      // The active terminal switched away while this follow-open was probing.
      return;
    }
    if (currentKey === key) {
      // Already viewing this server: refresh the browse sid but keep the
      // current directory — the initial directory only applies on first open.
      // (A pending paste conflict-check still matches: same viewShellId/path.)
      const changed = viewShellId !== sid;
      viewShellId = sid;
      if (changed) {
        entries = [];
        listTruncated = false;
      }
      refresh();
      return;
    }
    const saved = views.get(key);
    saveView();
    if (saved) {
      // Opened earlier this session: restore its directory.
      applyBrowse(key, sid, saved.path);
    } else {
      // First open: follow the terminal's known directory (or its home).
      const target = initialPath || "/";
      views.set(key, { sid, path: target });
      applyBrowse(key, sid, target);
    }
  }

  /** Immediately clear the listing and enter the loading state. Called by the
   *  parent when a new target (e.g. a saved server's SFTP) is requested, so
   *  the panel never keeps displaying the previous server's files. */
  export function prepareBrowse() {
    loading = true;
    listTruncated = false;
    entries = [];
    clearViewState();
  }

  /** Restore a previously-browsed view (shell + directory + server key) after
   *  a refresh. Called by the parent once the shell replay has settled. */
  export function applyRestoredView(p: string, sid: number, key: string) {
    saveView();
    views.set(key, { sid, path: p });
    applyBrowse(key, sid, p);
    // Suppress the "follow active server" reactive for the restored key so it
    // doesn't immediately override the restored view.
    prevActiveKey = activeKey;
  }

  /** Persist the current view (shell + directory) so a refresh returns here. */
  $: if (viewShellId != null) writeSftpView(path, viewShellId);

  function refresh() {
    if (!srocket || viewShellId === null) return;
    loading = true;
    listTruncated = false;
    srocket.send({ sftpList: [viewShellId, path] });
  }

  /** Navigate to `newPath`, pushing the current one onto the given stack
   *  (the back/forward history), then clearing per-directory state. Shared tail
   *  of all navigation helpers. */
  function navigateTo(newPath: string, pushTo: "history" | "forward") {
    if (pushTo === "history") history.push(path);
    else forward.push(path);
    path = newPath;
    clearViewState();
    refresh();
  }

  function enterDir(name: string) {
    navigateTo(join(path, name), "history");
  }

  function goUp() {
    if (path === "/") return;
    navigateTo(parentOf(path), "history");
  }

  /** Go back to the previously viewed directory (mouse back button). */
  function goBack() {
    if (history.length === 0) return;
    navigateTo(history.pop()!, "forward");
  }

  /** Re-enter a directory left via back navigation (mouse forward button). */
  function goForward() {
    if (forward.length === 0) return;
    navigateTo(forward.pop()!, "history");
  }

  /**
   * Repurpose the mouse side buttons (button 3 = back, button 4 = forward)
   * inside the file manager: instead of the browser navigating its own
   * history, they move between folders. `preventDefault` on mousedown /
   * mouseup / auxclick stops the browser's default back/forward action.
   */
  function onMouseNav(event: MouseEvent) {
    if (event.button !== 3 && event.button !== 4) return;
    event.preventDefault();
    if (event.type === "mousedown") {
      if (event.button === 3) goBack();
      else goForward();
    }
  }

  /** Enter edit mode for the path bar. */
  function startEditPath() {
    pathDraft = path;
    editingPath = true;
    requestAnimationFrame(() => {
      pathInput?.focus();
      pathInput?.select();
    });
  }

  /** Commit the edited path and navigate to it. */
  function commitPath() {
    if (!editingPath) return;
    editingPath = false;
    const target = pathDraft.trim();
    if (!target || target === path) return;
    navigateTo(target.startsWith("/") ? target : `/${target}`, "history");
  }

  // ---- Selection ---------------------------------------------------------
  function isSelected(name: string): boolean {
    return selected.has(name);
  }

  function selectOnly(name: string) {
    selected = new Set([name]);
    anchorName = name;
  }

  function toggleSelect(name: string) {
    selected = new Set(selected);
    if (selected.has(name)) selected.delete(name);
    else selected.add(name);
    anchorName = name;
  }

  /** Range-select from the anchor to the given entry (shift-click). */
  function rangeSelect(name: string) {
    const names = entries.map((e) => e.name);
    const from = anchorName ? names.indexOf(anchorName) : 0;
    const to = names.indexOf(name);
    if (from === -1 || to === -1) return;
    const [lo, hi] = from < to ? [from, to] : [to, from];
    selected = new Set(names.slice(lo, hi + 1));
  }

  /** Focus index for keyboard navigation. */
  let focusIndex = -1;
  let listEl: HTMLDivElement;

  function moveFocus(delta: number) {
    if (entries.length === 0) return;
    if (focusIndex === -1) focusIndex = 0;
    else
      focusIndex = Math.min(
        entries.length - 1,
        Math.max(0, focusIndex + delta),
      );
    const el = listEl?.querySelector<HTMLElement>(`[data-idx="${focusIndex}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }

  function onKeydown(event: KeyboardEvent) {
    // While renaming or an overwrite/skip decision is open, keystrokes belong
    // to that dialog/input (native Ctrl+C/X/V on the rename input still work).
    if (renamingName !== null || overwriteDialog) return;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        moveFocus(1);
        if (!event.shiftKey && !event.ctrlKey) {
          const e = entries[focusIndex];
          if (e) selectOnly(e.name);
        }
        break;
      case "ArrowUp":
        event.preventDefault();
        moveFocus(-1);
        if (!event.shiftKey && !event.ctrlKey) {
          const e = entries[focusIndex];
          if (e) selectOnly(e.name);
        }
        break;
      case "Enter":
        if (focusIndex >= 0 && focusIndex < entries.length) {
          const e = entries[focusIndex];
          if (e.isDir) enterDir(e.name);
        }
        break;
      case " ":
        if (focusIndex >= 0 && focusIndex < entries.length) {
          event.preventDefault();
          toggleSelect(entries[focusIndex].name);
        }
        break;
      case "F2":
        if (selected.size === 1) {
          startRename([...selected][0]);
        }
        break;
      case "Delete":
      case "Backspace":
        if (selected.size > 0) {
          event.preventDefault();
          deleteSelected();
        }
        break;
      case "a":
      case "A":
        if (event.ctrlKey || event.metaKey) {
          event.preventDefault();
          selected = new Set(entries.map((e) => e.name));
        }
        break;
      case "c":
      case "C":
        if (event.ctrlKey || event.metaKey) {
          event.preventDefault();
          copySelected();
        }
        break;
      case "x":
      case "X":
        if (event.ctrlKey || event.metaKey) {
          event.preventDefault();
          cutSelected();
        }
        break;
      case "v":
      case "V":
        if (event.ctrlKey || event.metaKey) {
          event.preventDefault();
          void pasteClipboard(keyboardPasteTargetDir());
        }
        break;
    }
  }

  // ---- Clipboard (copy / cut / paste) -----------------------------------
  let clipboard: { paths: string[]; mode: "copy" | "cut" } | null = null;
  /** Pending overwrite/skip decision for a paste that hit same-named items:
   *  the paste target directory and the conflicting source paths. */
  let overwriteDialog: { targetDir: string; conflicts: string[] } | null = null;
  /** A listing sent solely to resolve conflicts for a paste into a folder that
   *  isn't the current view; the reply is routed here, not to the view. */
  let pendingList: {
    path: string;
    resolve: (entries: WsSftpEntry[]) => void;
  } | null = null;

  /** Pending move/copy batch awaiting its `sftpOk` acks, so a final result
   *  toast ("剪切成功 N 项"/"复制成功 N 项") fires once the whole operation
   *  completes. Failures report their own error toast and clear the batch (a
   *  partial failure suppresses the success toast). */
  let moveCopyBatch: {
    kind: "move" | "copy";
    total: number;
    remaining: Set<string>;
  } | null = null;

  /** Track a move/copy batch keyed by the source path (the `sftpOk` ack
   *  path for rename/copy). */
  function beginMoveCopy(froms: string[], kind: "move" | "copy") {
    moveCopyBatch = { kind, total: froms.length, remaining: new Set(froms) };
  }

  /** Put the selected entries into the in-app clipboard as a copy or cut. */
  function setClipboard(mode: "copy" | "cut") {
    if (selected.size === 0) return;
    clipboard = {
      paths: [...selected].map((name) => join(path, name)),
      mode,
    };
    makeToast({
      kind: "info",
      message: t(
        $lang,
        mode === "copy" ? "file.toastClipCopied" : "file.toastClipCut",
      ),
    });
  }

  /** Copy the selected entries to the in-app clipboard. */
  function copySelected() {
    setClipboard("copy");
  }

  /** Cut the selected entries to the in-app clipboard. */
  function cutSelected() {
    setClipboard("cut");
  }

  /** The directory a context-menu paste targets: the right-clicked folder
   *  (when one), otherwise the current directory. */
  function pasteTargetDir(): string {
    if (ctxEntry?.isDir) return join(path, ctxEntry.name);
    return path;
  }

  /** The directory a Ctrl+V paste targets: exactly one folder selected → paste
   *  into it, otherwise the current directory. */
  function keyboardPasteTargetDir(): string {
    if (selected.size === 1) {
      const name = [...selected][0];
      const e = entries.find((x) => x.name === name);
      if (e?.isDir) return join(path, e.name);
    }
    return path;
  }

  /** Resolve the entries of the paste target directory so same-named items can
   *  be detected before pasting. The current view's entries are reused; a
   *  sub-folder is listed via a one-off `sftpList` whose reply is routed to
   *  `pendingList` (never to the view — see 坑 32). */
  function resolveTargetEntries(
    targetDir: string,
    shell: number,
  ): Promise<WsSftpEntry[]> {
    if (targetDir === path) return Promise.resolve(entries);
    return new Promise((resolve) => {
      pendingList = { path: targetDir, resolve };
      srocket?.send({ sftpList: [shell, targetDir] });
    });
  }

  /** Paste the clipboard into `targetDir`. When the target already holds
   *  same-named items, ask 覆盖 / 跳过 / 取消 before sending anything. */
  async function pasteClipboard(targetDir: string) {
    if (!clipboard || clipboard.paths.length === 0) return;
    if (!srocket || viewShellId === null) return;
    const shell = viewShellId;
    const existing = await resolveTargetEntries(targetDir, shell);
    const conflicts = clipboard.paths.filter((from) =>
      existing.some((e) => e.name === basename(from)),
    );
    if (conflicts.length > 0) {
      overwriteDialog = { targetDir, conflicts };
      return;
    }
    doPaste(targetDir, clipboard.paths);
  }

  /** Send the copy/move operations for `paths` into `targetDir` (an
   *  overwrite/skip decision has already been applied to `paths`). */
  function doPaste(targetDir: string, paths: string[]) {
    if (paths.length === 0) return;
    if (!clipboard || !srocket || viewShellId === null) return;
    const { mode } = clipboard;
    beginMoveCopy(paths, mode === "cut" ? "move" : "copy");
    for (const from of paths) {
      const name = basename(from);
      const to = join(targetDir, name);
      if (mode === "cut") {
        srocket.send({ sftpRename: [viewShellId, from, to] });
      } else {
        srocket.send({ sftpCopy: [viewShellId, from, to] });
      }
    }
    if (mode === "cut") clipboard = null;
  }

  /** Copy the absolute path of the selection (or current directory). */
  async function copyAbsolutePath() {
    let text: string;
    if (selected.size === 0) {
      text = path;
    } else {
      text = [...selected].map((name) => join(path, name)).join("\n");
    }
    const ok = await copyText(text);
    makeToast({
      kind: ok ? "success" : "error",
      message: ok
        ? t($lang, "file.toastCopyPath")
        : t($lang, "file.toastCopyFail"),
    });
  }

  /** Open a new SSH/local terminal in the directory (the right-clicked folder
   *  if any, otherwise the current directory). */
  function sshInCurrentDir() {
    const target =
      ctxEntry && ctxEntry.isDir ? join(path, ctxEntry.name) : path;
    dispatch("sshInDir", { dir: target, sid: viewShellId });
  }

  // ---- Custom context menu ----------------------------------------------
  let ctxMenu: { x: number; y: number } | null = null;
  /** The entry that was right-clicked (for folder-aware menu actions). */
  let ctxEntry: WsSftpEntry | null = null;

  function openCtxMenu(event: MouseEvent) {
    event.preventDefault();
    ctxMenu = { x: event.clientX, y: event.clientY };
  }

  function closeCtxMenu() {
    ctxMenu = null;
  }

  /** Route a context-menu action to the matching file operation. */
  function handleCtxAction(action: string) {
    switch (action) {
      case "newFile":
        newFile();
        break;
      case "newDir":
        mkdir();
        break;
      case "uploadFiles":
        fileInput?.click();
        break;
      case "uploadFolder":
        folderInput?.click();
        break;
      case "rename":
        if (selected.size === 1) startRename([...selected][0]);
        break;
      case "copy":
        copySelected();
        break;
      case "cut":
        cutSelected();
        break;
      case "paste":
        void pasteClipboard(pasteTargetDir());
        break;
      case "download":
        downloadSelected();
        break;
      case "delete":
        deleteSelected();
        break;
      case "copyPath":
        copyAbsolutePath();
        break;
      case "sshInDir":
        sshInCurrentDir();
        break;
    }
  }

  // ---- Drag & drop move -------------------------------------------------
  let dragSourceNames: string[] = [];
  let dropTargetName: string | null = null;
  /** The ".." (up) row is a drop target for moving into the parent directory;
   *  it highlights like a folder drop target while dragging over it. */
  let dropUpActive = false;

  function onEntryDragStart(name: string) {
    // Drag the whole selection; if the entry isn't selected, select it first.
    if (!isSelected(name)) selectOnly(name);
    dragSourceNames = [...selected];
  }

  function onEntryDragEnd() {
    dragSourceNames = [];
    dropTargetName = null;
    dropUpActive = false;
  }

  /** Move the dragged entries into `targetDir`. */
  function moveInto(targetDir: string) {
    if (!srocket || viewShellId === null || dragSourceNames.length === 0)
      return;
    if (targetDir === path) return;
    beginMoveCopy(
      dragSourceNames.map((name) => join(path, name)),
      "move",
    );
    for (const name of dragSourceNames) {
      srocket.send({
        sftpRename: [viewShellId, join(path, name), join(targetDir, name)],
      });
    }
    selected = new Set();
  }

  function onDropTargetDragOver(
    entry: WsSftpEntry,
    event?: DragEvent,
  ): boolean {
    // External OS files/folders → allow dropping onto this folder (upload).
    if (event?.dataTransfer?.types?.includes("Files")) {
      dropTargetName = entry.name;
      return true;
    }
    if (dragSourceNames.length === 0 || !entry.isDir) return false;
    dropTargetName = entry.name;
    return true;
  }

  function onDropTargetDragLeave() {
    dropTargetName = null;
  }

  function onDropTargetDrop(entry: WsSftpEntry, event?: DragEvent) {
    // External files/folders dragged in from the OS → upload into this folder.
    if (event?.dataTransfer?.files?.length) {
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, join(path, entry.name));
      return;
    }
    if (!entry.isDir || dragSourceNames.includes(entry.name)) return;
    moveInto(join(path, entry.name));
  }

  /** Drop onto the ".." (parent directory) row. */
  function onUpRowDrop(event?: DragEvent) {
    // External files/folders dragged in → upload into the parent directory.
    if (event?.dataTransfer?.files?.length) {
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, parentOf(path));
      return;
    }
    if (dragSourceNames.length === 0 || path === "/") return;
    moveInto(parentOf(path));
  }

  function onUpRowDragOver(event?: DragEvent): boolean {
    // Accept external OS drags too (dropEffect handled by the row's droppable).
    if (event?.dataTransfer?.types?.includes("Files")) {
      dropUpActive = true;
      return true;
    }
    dropUpActive = dragSourceNames.length > 0 && path !== "/";
    return dropUpActive;
  }

  function onUpRowDragLeave() {
    dropUpActive = false;
  }

  // ---- Actions -----------------------------------------------------------
  /** Trigger a native browser download of a single file via the HTTP Range
   *  endpoint. The browser handles pause/resume/cancel without buffering in
   *  the page, so downloads are not tracked in the transfer panel. */
  function download(entry: WsSftpEntry) {
    if (viewShellId === null || !sessionName) return;
    const targetPath = join(path, entry.name);
    triggerBrowserDownload(
      `${sftpHttpPath(
        sessionName,
        viewShellId,
      )}/download?path=${encodeURIComponent(targetPath)}`,
      entry.name,
    );
  }

  /** Download selected items.
   *
   * A single file downloads directly via HTTP (native browser download);
   * multiple files or any folder are packaged into a ZIP archive **streamed
   * from the server** through the HTTP archive endpoint (no server-side temp
   * file, no whole-archive buffering in memory). Neither path is tracked in
   * the transfer panel.
   */
  function downloadSelected() {
    if (selected.size === 0) return;
    if (viewShellId === null || !sessionName) return;
    const selectedEntries = [...selected]
      .map((name) => entries.find((x) => x.name === name))
      .filter((e): e is WsSftpEntry => Boolean(e));

    // Single file: download directly with the original name.
    if (selectedEntries.length === 1 && selectedEntries[0].isDir === false) {
      download(selectedEntries[0]);
      return;
    }

    // Multiple files, or any folder: stream a ZIP archive to the browser.
    if (selectedEntries.length === 0) {
      makeToast({ kind: "error", message: t($lang, "file.toastNothing") });
      return;
    }
    const name = archiveNameFor(selectedEntries);
    const paths = selectedEntries.map((e) => join(path, e.name));
    // A single selected folder is archived flat: its contents become the
    // top-level zip entries (no extra `folder/` wrapper layer).
    const flat =
      selectedEntries.length === 1 && selectedEntries[0].isDir
        ? "flat=1"
        : null;
    const url = `${sftpHttpPath(sessionName, viewShellId)}/archive?${[
      ...paths.map((p) => `path=${encodeURIComponent(p)}`),
      `filename=${encodeURIComponent(name)}`,
      ...(flat ? [flat] : []),
    ].join("&")}`;
    triggerBrowserDownload(url, name);
  }

  /** Request deletion: opens the custom confirmation dialog. */
  function deleteSelected() {
    if (!srocket || viewShellId === null) return;
    if (selected.size === 0) return;
    deleteTarget = [...selected];
  }

  function confirmDelete() {
    if (!deleteTarget || !srocket || viewShellId === null) return;
    const names = deleteTarget;
    // Track pending deletions so the sftpOk acks can report "已删除" once all
    // succeed (and failures surface an error toast).
    pendingDeletes = new Set(names.map((name) => join(path, name)));
    pendingDeleteNames = names;
    for (const name of names) {
      const e = entries.find((x) => x.name === name);
      srocket.send({
        sftpRemove: [viewShellId, join(path, name), e ? e.isDir : false],
      });
    }
    selected = new Set();
    deleteTarget = null;
  }

  /** Open a file in the editor by reading it from the server. */
  function openEditor(entry: WsSftpEntry) {
    if (!srocket || viewShellId === null) return;
    dispatch("openEditor", { path: join(path, entry.name), sid: viewShellId });
  }

  function startRename(name: string) {
    renamingName = name;
    renameValue = name;
    selectOnly(name);
    requestAnimationFrame(() => renameInput?.select());
  }

  function cancelRename() {
    renamingName = null;
  }

  function commitRename() {
    if (renamingName === null) return;
    const oldName = renamingName;
    const newName = renameValue.trim();
    renamingName = null;
    if (!newName || newName === oldName) return;
    if (!srocket || viewShellId === null) return;
    srocket.send({
      sftpRename: [viewShellId, join(path, oldName), join(path, newName)],
    });
  }

  /** Open the "new directory"/"new file" name dialog. */
  function openPrompt(kind: "file" | "dir") {
    promptDialog = {
      kind,
      title: t(
        $lang,
        kind === "dir" ? "file.newDirTitle" : "file.newFileTitle",
      ),
    };
    promptValue = "";
  }

  /** Open the "new directory" name dialog. */
  function mkdir() {
    openPrompt("dir");
  }

  /** Open the "new file" name dialog. */
  function newFile() {
    openPrompt("file");
  }

  function confirmPrompt(name: string) {
    if (!promptDialog || !srocket || viewShellId === null) return;
    const { kind } = promptDialog;
    promptDialog = null;
    const trimmed = name.trim();
    if (!trimmed) return;
    if (kind === "dir") {
      srocket.send({ sftpMkdir: [viewShellId, join(path, trimmed)] });
    } else {
      srocket.send({
        sftpWrite: [viewShellId, join(path, trimmed), new Uint8Array()],
      });
    }
  }

  /** Upload a file in acknowledged chunks. `destPath` overrides the default
   *  current-directory target (used for folder uploads to preserve the
   *  sub-directory structure). */
  function upload(file: File, destPath: string, displayName: string) {
    if (!srocket || viewShellId === null) return;
    startUpload({
      file,
      destPath,
      displayName,
      targetShell: viewShellId,
      targetName: targetNames[viewShellId] ?? "",
      socket: srocket,
      onDone: refresh,
    });
  }

  /** Upload files into the current directory; folder uploads (webkitdirectory,
   *  from the folder picker) preserve the sub-directory layout via
   *  `webkitRelativePath`. (OS drag-drops go through `uploadDropped` with a
   *  recursive walk instead — `webkitRelativePath` is not populated on drops.) */
  function uploadIntoDir(files: File[], folder = false) {
    for (const file of files) {
      if (folder && file.webkitRelativePath) {
        const parts = file.webkitRelativePath.split("/");
        const top = parts[0];
        const rest = parts.slice(1).join("/");
        upload(file, join(join(path, top), rest), file.webkitRelativePath);
      } else {
        upload(file, join(path, file.name), file.name);
      }
    }
  }

  /** Upload several files into the current directory. */
  function uploadFiles(files: File[]) {
    uploadIntoDir(files);
  }

  /** Upload a folder (webkitdirectory) preserving its sub-directory layout. */
  function uploadFolder(files: File[]) {
    uploadIntoDir(files, true);
  }

  /** Shared change handler for the file / folder pickers: feed the chosen
   *  files to `fn` and reset the input so re-picking the same files re-fires. */
  function handleInputChange(event: Event, fn: (files: File[]) => void) {
    const input = event.currentTarget as HTMLInputElement;
    fn(input.files ? [...input.files] : []);
    input.value = "";
  }

  function handleFilesChange(event: Event) {
    handleInputChange(event, uploadFiles);
  }

  function handleFolderChange(event: Event) {
    handleInputChange(event, uploadFolder);
  }

  /** Handle files/folders dropped from the OS onto `targetDir`. Uses
   *  `webkitGetAsEntry` (via `collectDropFiles`) so a dropped folder's whole
   *  tree is uploaded with its sub-directory layout — a naive
   *  `dataTransfer.files` read loses folder structure and can even upload the
   *  folder itself as a 0-byte file. */
  async function uploadDropped(
    payload: DropPayload,
    targetDir: string,
  ): Promise<void> {
    const dropped = await collectDropFiles(payload);
    for (const { file, relPath } of dropped) {
      upload(file, join(targetDir, relPath), relPath);
    }
  }

  /** Svelte action: mark an input as a folder picker (webkitdirectory). */
  function folderPicker(node: HTMLInputElement) {
    (node as unknown as { webkitdirectory: boolean }).webkitdirectory = true;
    return {};
  }

  /** Handle an SFTP-related server message routed from the parent. */
  export function handleMessage(message: WsServer) {
    if (message.sftpList) {
      const [listShell, listPath, list, truncated] = message.sftpList;
      // Only accept a listing that matches the current target AND directory.
      // List requests are spawned independently on the server, so a slow,
      // overlapping response for a previous directory (rapid double-clicks)
      // must not overwrite the current view.
      if (listShell === viewShellId && listPath === path) {
        applyListing(list, truncated, listPath);
      } else if (
        pendingList &&
        listShell === viewShellId &&
        listPath === pendingList.path
      ) {
        // A listing requested solely to resolve paste conflicts: hand its
        // entries to the pending paste, never to the current view.
        resolvePendingList(list);
      }
    } else if (message.sftpOk) {
      applyAck(message.sftpOk);
    } else if (message.sftpWriteOk) {
      // A chunked-upload ack: carries the written offset for dedup/resume.
      applyWriteAck(message.sftpWriteOk);
    } else if (message.error) {
      applyError(message.error);
    }
  }

  /** Apply a listing for the current view. The user may have navigated into
   *  the very folder a paste is conflict-checking: that view listing also
   *  satisfies the pending check (otherwise the paste would wait forever). */
  function applyListing(
    list: WsSftpEntry[],
    truncated: boolean,
    listPath: string,
  ) {
    entries = sortEntries(list);
    listTruncated = truncated;
    loading = false;
    focusIndex = -1;
    if (pendingList && pendingList.path === listPath) {
      resolvePendingList(list);
    }
  }

  /** Apply an `sftpOk` acknowledgement: an upload chunk ack or the final ack
   *  of a move/copy batch (rename/copy report the source path). */
  function applyAck([savedShell, savedPath]: [number, string]) {
    if (pendingDeletes.has(savedPath)) {
      // A deletion succeeded: once every pending delete is acked, report it.
      pendingDeletes.delete(savedPath);
      if (pendingDeletes.size === 0) {
        const deleted = pendingDeleteNames;
        pendingDeleteNames = [];
        makeToast({
          kind: "success",
          message:
            deleted.length === 1
              ? t($lang, "file.toastDeleted", { name: deleted[0] })
              : t($lang, "file.toastDeletedMany", { n: deleted.length }),
        });
      }
      if (savedShell === viewShellId) {
        refresh();
      }
      return;
    }
    if (!onUploadAck(savedShell, savedPath)) {
      const batch = moveCopyBatch;
      if (batch && batch.remaining.delete(savedPath)) {
        if (batch.remaining.size === 0) {
          moveCopyBatch = null;
          makeToast({
            kind: "success",
            message: t(
              $lang,
              batch.kind === "move"
                ? "file.toastPasteMoved"
                : "file.toastPasteCopied",
              { n: batch.total },
            ),
          });
        }
      }
      if (savedShell === viewShellId) {
        refresh();
      }
    }
  }

  /** Apply a chunked-upload acknowledgement (`sftpWriteOk`, which echoes the
   *  written offset). Write-oks only ever acknowledge uploads — never move/copy
   *  batches or deletions — so there is no fallback handling here. */
  function applyWriteAck([savedShell, savedPath, offset]: [
    number,
    string,
    number,
  ]) {
    onUploadAck(savedShell, savedPath, offset);
  }

  /** Apply an `error` message from the server. */
  function applyError(message: string) {
    // A failed conflict-check listing would otherwise leave the paste
    // awaiting forever; fall back to pasting without the overwrite prompt.
    resolvePendingList();
    // A deletion failed: surface the error and stop waiting for its acks.
    if (message.startsWith("删除失败")) {
      pendingDeletes = new Set();
      pendingDeleteNames = [];
    }
    makeToast({ kind: "error", message });
    loading = false;
    // A "写入失败（<path>）：..." error marks a failed upload chunk.
    onUploadError(message);
    // A failed move/copy shows its own error toast; drop the batch so no
    // "复制成功/剪切成功" toast is emitted for a partial failure.
    if (message.startsWith("重命名失败") || message.startsWith("复制失败")) {
      moveCopyBatch = null;
    }
  }

  onMount(() => {
    refresh();
  });

  /** Timer for the hover tooltip delay (0.5s). */
  let hoverTimer: ReturnType<typeof setTimeout> | null = null;
  /** The entry a pending/active hover is targeting. */
  let hoverTarget: WsSftpEntry | null = null;

  /** Called on mouse enter / move: schedule or refresh the tooltip. */
  function showHover(event: MouseEvent, entry: WsSftpEntry) {
    hoverTarget = entry;
    // If already shown for this entry, just follow the cursor.
    if (hoverEntry === entry) {
      hoverX = Math.min(event.clientX + 12, window.innerWidth - 260);
      hoverY = event.clientY + 12;
      return;
    }
    if (hoverTimer) clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => {
      if (hoverTarget === entry) {
        hoverEntry = entry;
        hoverX = Math.min(event.clientX + 12, window.innerWidth - 260);
        hoverY = event.clientY + 12;
      }
      hoverTimer = null;
    }, TOOLTIP_DELAY_MS);
  }

  function hideHover() {
    hoverTarget = null;
    if (hoverTimer) {
      clearTimeout(hoverTimer);
      hoverTimer = null;
    }
    hoverEntry = null;
  }

  /** Accept external OS file drags over the panel body (folder rows handle
   *  their own drop via their droppable). */
  function onSidebarDragOver(event: DragEvent) {
    if (event.dataTransfer?.types?.includes("Files")) {
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
    }
  }

  /** Dropping files/folders on the panel body uploads them into the current
   *  directory (folder rows stop propagation and upload into themselves). */
  function onSidebarDrop(event: DragEvent) {
    if (event.dataTransfer?.files?.length) {
      event.preventDefault();
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, path);
    }
  }
</script>

<Sidebar
  resize={sidebarResize}
  {open}
  showHandle={open}
  on:mousedown={onMouseNav}
  on:mouseup={onMouseNav}
  on:auxclick={onMouseNav}
  on:dragover={onSidebarDragOver}
  on:drop={onSidebarDrop}
>
  <!-- Path bar + refresh -->
  <div class="flex h-9 items-center gap-1 border-b border-zinc-800 px-3">
    <button
      class="shrink-0 text-zinc-400 transition-colors hover:text-zinc-200"
      on:click={refresh}
      title={t($lang, "file.titleRefresh")}
    >
      <RefreshCwIcon size="16" />
    </button>
    {#if editingPath}
      <input
        bind:this={pathInput}
        bind:value={pathDraft}
        class="ml-1 flex-1 rounded border border-indigo-500 bg-zinc-900 px-1 py-0.5 font-mono text-xs text-zinc-100 outline-none"
        use:enterEscape={{
          onEnter: commitPath,
          onEscape: () => (editingPath = false),
          stopPropagation: true,
        }}
        on:blur={commitPath}
      />
    {:else}
      <button
        class="ml-1 flex-1 truncate text-left font-mono text-xs text-zinc-300 hover:text-zinc-100"
        on:click={startEditPath}
        title={t($lang, "file.titleEditPath")}
      >
        {path}
      </button>
    {/if}
  </div>

  <!-- Search row: helps navigate very large directories without rendering
       every row (which would freeze the browser). -->
  <div class="flex h-8 items-center gap-1 border-b border-zinc-800 px-3">
    <SearchIcon size="13" class="shrink-0 text-zinc-500" />
    <input
      bind:value={searchQuery}
      class="min-w-0 flex-1 bg-transparent text-xs text-zinc-300 outline-none placeholder:text-zinc-600"
      placeholder={entries.length > MAX_FILE_ROWS
        ? t($lang, "file.searchN", { n: entries.length })
        : t($lang, "file.searchHint", {
            target:
              targetNames[viewShellId ?? -1] ?? t($lang, "file.currentSession"),
          })}
      on:keydown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          searchQuery = "";
        }
      }}
    />
    {#if searchQuery}
      <button
        class="shrink-0 text-zinc-500 transition-colors hover:text-zinc-200"
        on:click={() => (searchQuery = "")}
        title={t($lang, "file.titleClearSearch")}
      >
        <XIcon size="12" />
      </button>
    {/if}
  </div>

  {#if listTruncated}
    <div
      class="border-b border-amber-900/40 bg-amber-900/20 px-3 py-1 text-[10px] text-amber-300"
      title={t($lang, "file.truncated")}
    >
      {t($lang, "file.truncated")}
    </div>
  {/if}

  <!-- File list -->
  <div
    class="no-scrollbar flex-1 overflow-y-auto outline-none transition-opacity duration-150"
    class:opacity-60={loading}
    bind:this={listEl}
    tabindex="0"
    role="listbox"
    aria-label={t($lang, "file.list")}
    on:keydown={onKeydown}
    on:contextmenu={(event) => {
      ctxEntry = null;
      openCtxMenu(event);
    }}
  >
    {#if loading && entries.length === 0}
      <div class="flex items-center gap-2 px-3 py-2 text-xs text-zinc-500">
        <RefreshCwIcon size="12" class="animate-spin" />
        {t($lang, "file.loading")}
      </div>
    {/if}
    <!-- 上级目录入口 (双击返回,与普通文件夹一致) -->
    {#if path !== "/"}
      <div
        class="flex cursor-pointer select-none items-center gap-2 px-3 py-1.5 text-sm text-zinc-400 transition-colors hover:bg-zinc-800 {dropUpActive
          ? 'bg-indigo-600/40 shadow-[inset_3px_0_0_0_#818cf8]'
          : ''}"
        role="button"
        tabindex="-1"
        on:dblclick={goUp}
        on:keydown={noop}
        use:droppable={{
          onDragOver: onUpRowDragOver,
          onDrop: onUpRowDrop,
          onDragLeave: onUpRowDragLeave,
        }}
        title={t($lang, "file.upDir")}
      >
        <FolderIcon size="16" class="shrink-0 text-amber-400" />
        <span class="flex-1 truncate font-mono">...</span>
      </div>
    {/if}

    {#each visibleEntries as entry, idx (entry.name)}
      {@const type = fileType(entry.name)}
      <div
        data-idx={idx}
        role="option"
        tabindex="-1"
        aria-selected={selected.has(entry.name)}
        draggable="true"
        class="group flex cursor-pointer select-none items-center gap-2 px-3 py-1.5 text-sm transition-colors {selected.has(
          entry.name,
        )
          ? 'bg-[#2A4371] text-zinc-100 shadow-[inset_3px_0_0_0_#60a5fa]'
          : 'text-zinc-300 hover:bg-zinc-800'} {dropTargetName === entry.name
          ? 'bg-sky-600/40 shadow-[inset_3px_0_0_0_#7dd3fc]'
          : ''}"
        on:keydown={noop}
        use:draggable={{
          key: entry.name,
          onStart: onEntryDragStart,
          onEnd: onEntryDragEnd,
        }}
        use:droppable={{
          onDragOver: (event) => onDropTargetDragOver(entry, event),
          onDrop: (event) => onDropTargetDrop(entry, event),
          onDragLeave: onDropTargetDragLeave,
        }}
        on:click={(event) => {
          if (event.ctrlKey || event.metaKey) toggleSelect(entry.name);
          else if (event.shiftKey) rangeSelect(entry.name);
          else selectOnly(entry.name);
        }}
        on:contextmenu|stopPropagation={(event) => {
          event.preventDefault();
          if (event.ctrlKey || event.metaKey) toggleSelect(entry.name);
          else if (event.shiftKey) rangeSelect(entry.name);
          else if (!isSelected(entry.name)) selectOnly(entry.name);
          ctxEntry = entry;
          openCtxMenu(event);
        }}
        on:dblclick={() => {
          if (entry.isDir) enterDir(entry.name);
          else openEditor(entry);
        }}
        on:mousemove={(e) => showHover(e, entry)}
        on:mouseleave={hideHover}
      >
        {#if entry.isDir}
          <FolderIcon size="16" class="shrink-0 text-amber-400" />
        {:else}
          <svelte:component
            this={type.icon}
            size="16"
            class={`shrink-0 ${type.color}`}
          />
        {/if}

        {#if renamingName === entry.name}
          <input
            bind:this={renameInput}
            bind:value={renameValue}
            class="flex-1 rounded border border-indigo-500 bg-zinc-900 px-1 py-0.5 text-sm text-zinc-100 outline-none"
            use:enterEscape={{
              onEnter: commitRename,
              onEscape: cancelRename,
              stopPropagation: true,
            }}
            on:click|stopPropagation
            on:blur={commitRename}
          />
        {:else}
          <!-- No `title` here: the custom hover tooltip already shows the
               full name; the browser's native `<title>` would pop up later
               and cover it. The `flex-1 truncate` ellipsis happens exactly at
               the edge of the available space (no early JS truncation), so a
               name is never cut short of the permission column. -->
          <span
            class="min-w-0 flex-1 truncate"
            class:text-sky-300={entry.isLink}
          >
            {entry.name}
          </span>
        {/if}

        <span class="w-16 shrink-0 ml-1.5 font-mono text-[10px] text-zinc-600">
          {formatMode(entry.mode)}
        </span>

        <span class="w-20 shrink-0 text-right text-xs text-zinc-500">
          {entry.isDir ? "" : formatSize(entry.size)}
        </span>
      </div>
    {/each}
  </div>

  <!-- Hover info tooltip -->
  {#if hoverEntry}
    <FileTooltip entry={hoverEntry} x={hoverX} y={hoverY} />
  {/if}

  <!-- Custom context menu -->
  {#if ctxMenu}
    <ContextMenu
      x={ctxMenu.x}
      y={ctxMenu.y}
      selectedCount={selected.size}
      canPaste={!!clipboard && clipboard.paths.length > 0}
      pasteMove={clipboard?.mode === "cut"}
      pasteTargetName={ctxEntry?.isDir ? ctxEntry.name : null}
      onClose={closeCtxMenu}
      onAction={handleCtxAction}
    />
  {/if}

  <!-- Transfer task panel (bottom-right) -->
  <UploadPanel />
</Sidebar>

<!-- Hidden pickers: files (multiple) and folders (webkitdirectory) -->
<input
  type="file"
  multiple
  class="hidden"
  bind:this={fileInput}
  on:change={handleFilesChange}
/>
<input
  type="file"
  multiple
  class="hidden"
  bind:this={folderInput}
  use:folderPicker
  on:change={handleFolderChange}
/>

<ConfirmDialog
  open={deleteTarget !== null}
  title={t($lang, "file.delTitle")}
  message={deleteTarget
    ? t($lang, "file.delMessage", { n: deleteTarget.length })
    : ""}
  danger
  confirmText={t($lang, "common.delete")}
  on:confirm={confirmDelete}
  on:cancel={() => (deleteTarget = null)}
/>

<PromptDialog
  open={promptDialog !== null}
  title={promptDialog?.title ?? ""}
  message={promptDialog?.kind === "dir"
    ? t($lang, "file.newDirMsg")
    : t($lang, "file.newFileMsg")}
  label={promptDialog?.kind === "dir"
    ? t($lang, "file.newDirLabel")
    : t($lang, "file.newFileLabel")}
  bind:value={promptValue}
  placeholder={promptDialog?.kind === "dir"
    ? t($lang, "file.newDirPh")
    : t($lang, "file.newFilePh")}
  confirmText={t($lang, "common.ok")}
  on:confirm={(event) => confirmPrompt(event.detail)}
  on:cancel={() => (promptDialog = null)}
/>

<OverwriteDialog
  open={overwriteDialog !== null}
  names={overwriteDialog?.conflicts.map((p) => basename(p)) ?? []}
  title={t($lang, "file.overwriteTitle")}
  on:overwrite={() => {
    if (overwriteDialog && clipboard) {
      doPaste(overwriteDialog.targetDir, clipboard.paths);
    }
    overwriteDialog = null;
  }}
  on:skip={() => {
    if (overwriteDialog && clipboard) {
      const skip = new Set(overwriteDialog.conflicts);
      doPaste(
        overwriteDialog.targetDir,
        clipboard.paths.filter((p) => !skip.has(p)),
      );
    }
    overwriteDialog = null;
  }}
  on:cancel={() => (overwriteDialog = null)}
/>

<svelte:window on:click={closeCtxMenu} />
