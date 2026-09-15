<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import { FolderIcon, RefreshCwIcon, SearchIcon, XIcon } from "$lib/icons";

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
    onUploadVerified,
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
  import ContextMenu from "./ContextMenu.svelte";
  import UploadPanel from "./UploadPanel.svelte";
  import FileTooltip from "./FileTooltip.svelte";
  import Sidebar from "./Sidebar.svelte";

  const dispatch = createEventDispatcher<{
    sshInDir: { dir: string; sid: number | null };
    openEditor: { path: string; sid: number };
    followActive: void;
  }>();

  const join = joinPath;

  interface Props {
    srocket: Srocket<WsServer, WsClient> | null;
    shellId: number | null;
    shellServerKeys?: Record<number, string>;
    sessionName?: string;
    targetNames?: Record<number, string>;
    open?: boolean;
    replaySettled?: boolean;
  }

  let {
    srocket,
    shellId,
    shellServerKeys = {},
    sessionName = "",
    targetNames = {},
    open = true,
    replaySettled = false,
  }: Props = $props();

  // ---- State -------------------------------------------------------------
  let path = $state("/");
  let entries = $state<WsSftpEntry[]>([]);
  let listTruncated = $state(false);
  let loading = $state(false);
  let history = $state<string[]>([]);
  let forward = $state<string[]>([]);

  let searchQuery = $state("");
  let query = $derived(searchQuery.trim().toLowerCase());
  let filtered = $derived(
    query
      ? entries.filter((e) => e.name.toLowerCase().includes(query))
      : entries,
  );
  let visibleEntries = $derived(filtered.slice(0, MAX_FILE_ROWS));

  let editingPath = $state(false);
  let pathDraft = $state("/");
  let pathInput = $state<HTMLInputElement>();

  let renamingName = $state<string | null>(null);
  let renameValue = $state("");
  let renameInput = $state<HTMLInputElement>();

  let fileInput = $state<HTMLInputElement>();
  let folderInput = $state<HTMLInputElement>();

  let selected = $state<Set<string>>(new Set());
  let anchorName = $state<string | null>(null);

  let hoverEntry = $state<WsSftpEntry | null>(null);
  let hoverX = $state(0);
  let hoverY = $state(0);

  let deleteTarget = $state<string[] | null>(null);
  let pendingDeletes = $state<Set<string>>(new Set());
  let pendingDeleteNames = $state<string[]>([]);
  let promptDialog = $state<{ kind: "file" | "dir"; title: string } | null>(
    null,
  );
  let promptValue = $state("");

  function sortEntries(list: WsSftpEntry[]): WsSftpEntry[] {
    return [...list].sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
  }

  let viewShellId = $state<number | null>(null);
  let views = new Map<string, { sid: number; path: string }>();
  let currentKey = $state<string | null>(null);
  let activeKey = $derived(
    shellId != null ? (shellServerKeys[shellId] ?? null) : null,
  );
  let prevActiveKey = $state<string | null>(null);
  let prevFollowOpen = $state(false);

  function saveView() {
    if (currentKey !== null && viewShellId !== null) {
      views.set(currentKey, { sid: viewShellId, path });
    }
  }

  function resolvePendingList(entries: WsSftpEntry[] = []) {
    if (pendingList) {
      pendingList.resolve(entries);
      pendingList = null;
    }
  }

  function applyBrowse(key: string, sid: number, targetPath: string) {
    const serverChanged = currentKey !== key;
    viewShellId = sid;
    currentKey = key;
    path = targetPath;
    if (serverChanged) {
      history = [];
      entries = [];
      listTruncated = false;
    }
    clearViewState();
    resolvePendingList();
    refresh();
  }

  $effect(() => {
    if (activeKey !== prevActiveKey || (open && !prevFollowOpen)) {
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
          saveView();
          dispatch("followActive");
        }
      }
    }
  });

  $effect(() => {
    if (viewShellId != null) saveView();
  });

  $effect(() => {
    if (
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
  });

  function clearViewState() {
    forward = [];
    selected = new Set();
    anchorName = null;
    searchQuery = "";
  }

  export function browseShell(
    sid: number,
    initialPath: string,
    key: string,
    explicit = true,
  ) {
    if (!explicit && key !== activeKey) {
      return;
    }
    if (currentKey === key) {
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
      applyBrowse(key, sid, saved.path);
    } else {
      const target = initialPath || "/";
      views.set(key, { sid, path: target });
      applyBrowse(key, sid, target);
    }
  }

  export function prepareBrowse() {
    loading = true;
    listTruncated = false;
    entries = [];
    clearViewState();
  }

  export function applyRestoredView(p: string, sid: number, key: string) {
    saveView();
    views.set(key, { sid, path: p });
    applyBrowse(key, sid, p);
    prevActiveKey = activeKey;
  }

  $effect(() => {
    if (viewShellId != null) writeSftpView(path, viewShellId);
  });

  function refresh() {
    if (!srocket || viewShellId === null) return;
    loading = true;
    listTruncated = false;
    srocket.send({ sftpList: [viewShellId, path] });
  }

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

  function goBack() {
    if (history.length === 0) return;
    navigateTo(history.pop()!, "forward");
  }

  function goForward() {
    if (forward.length === 0) return;
    navigateTo(forward.pop()!, "history");
  }

  function onMouseNav(event: MouseEvent) {
    if (event.button !== 3 && event.button !== 4) return;
    event.preventDefault();
    if (event.type === "mousedown") {
      if (event.button === 3) goBack();
      else goForward();
    }
  }

  function startEditPath() {
    pathDraft = path;
    editingPath = true;
    requestAnimationFrame(() => {
      pathInput?.focus();
      pathInput?.select();
    });
  }

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

  function rangeSelect(name: string) {
    const names = entries.map((e) => e.name);
    const from = anchorName ? names.indexOf(anchorName) : 0;
    const to = names.indexOf(name);
    if (from === -1 || to === -1) return;
    const [lo, hi] = from < to ? [from, to] : [to, from];
    selected = new Set(names.slice(lo, hi + 1));
  }

  /** Clear the selection when the click lands on empty list space (anywhere
   *  that is not a row). Rows match `[role="option"]`, so their own handlers
   *  are unaffected — this only fills the gap they leave. */
  function onListClick(event: MouseEvent) {
    const target = event.target instanceof Element ? event.target : null;
    if (target?.closest('[role="option"]')) return;
    if (selected.size === 0) return;
    selected = new Set();
    anchorName = null;
  }

  let focusIndex = $state(-1);
  let listEl = $state<HTMLDivElement>();

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

  // ---- Clipboard ---------------------------------------------------------
  let clipboard = $state<{ paths: string[]; mode: "copy" | "cut" } | null>(
    null,
  );
  let overwriteDialog = $state<{
    targetDir: string;
    conflicts: string[];
  } | null>(null);
  /** Overwrite-conflict preview + message (single call site; kept here so the
   *  i18n separator / overflow suffix stay in one place). */
  let overwriteNames = $derived(
    (overwriteDialog?.conflicts ?? []).map((p) => basename(p)),
  );
  let overwritePreview = $derived(
    overwriteNames.length <= 4
      ? overwriteNames.join(t($lang, "file.zipSeparator"))
      : `${overwriteNames.slice(0, 4).join(t($lang, "file.zipSeparator"))}…`,
  );
  let overwriteMessage = $derived(
    overwriteNames.length > 0
      ? t($lang, "file.overwriteMessage", {
          n: overwriteNames.length,
          names: overwritePreview,
        })
      : "",
  );
  let pendingList: {
    path: string;
    resolve: (entries: WsSftpEntry[]) => void;
  } | null = null;
  let moveCopyBatch = $state<{
    kind: "move" | "copy";
    total: number;
    remaining: Set<string>;
  } | null>(null);
  let copyBytesByFrom = $state(new Map<string, number>());
  let copyBytesTotal = $derived(
    [...copyBytesByFrom.values()].reduce((a, b) => a + b, 0),
  );

  function beginMoveCopy(froms: string[], kind: "move" | "copy") {
    moveCopyBatch = { kind, total: froms.length, remaining: new Set(froms) };
    copyBytesByFrom = new Map();
  }

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

  function copySelected() {
    setClipboard("copy");
  }

  function cutSelected() {
    setClipboard("cut");
  }

  function pasteTargetDir(): string {
    if (ctxEntry?.isDir) return join(path, ctxEntry.name);
    return path;
  }

  function keyboardPasteTargetDir(): string {
    if (selected.size === 1) {
      const name = [...selected][0];
      const e = entries.find((x) => x.name === name);
      if (e?.isDir) return join(path, e.name);
    }
    return path;
  }

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

  function sshInCurrentDir() {
    const target =
      ctxEntry && ctxEntry.isDir ? join(path, ctxEntry.name) : path;
    dispatch("sshInDir", { dir: target, sid: viewShellId });
  }

  // ---- Custom context menu ----------------------------------------------
  let ctxMenu = $state<{ x: number; y: number } | null>(null);
  let ctxEntry = $state<WsSftpEntry | null>(null);

  function openCtxMenu(event: MouseEvent) {
    event.preventDefault();
    ctxMenu = { x: event.clientX, y: event.clientY };
  }

  function closeCtxMenu() {
    ctxMenu = null;
  }

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
  let dropTargetName = $state<string | null>(null);
  let dropUpActive = $state(false);

  function onEntryDragStart(name: string) {
    if (!isSelected(name)) selectOnly(name);
    dragSourceNames = [...selected];
  }

  function onEntryDragEnd() {
    dragSourceNames = [];
    dropTargetName = null;
    dropUpActive = false;
  }

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
    if (event?.dataTransfer?.files?.length) {
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, join(path, entry.name));
      return;
    }
    if (!entry.isDir || dragSourceNames.includes(entry.name)) return;
    moveInto(join(path, entry.name));
  }

  function onUpRowDrop(event?: DragEvent) {
    if (event?.dataTransfer?.files?.length) {
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, parentOf(path));
      return;
    }
    if (dragSourceNames.length === 0 || path === "/") return;
    moveInto(parentOf(path));
  }

  function onUpRowDragOver(event?: DragEvent): boolean {
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
  function download(entry: WsSftpEntry) {
    if (viewShellId === null || !sessionName) return;
    const targetPath = join(path, entry.name);
    triggerBrowserDownload(
      `${sftpHttpPath(sessionName, viewShellId)}/download?path=${encodeURIComponent(
        targetPath,
      )}`,
      entry.name,
    );
  }

  function downloadSelected() {
    if (selected.size === 0) return;
    if (viewShellId === null || !sessionName) return;
    const selectedEntries = [...selected]
      .map((name) => entries.find((x) => x.name === name))
      .filter((e): e is WsSftpEntry => Boolean(e));
    if (selectedEntries.length === 1 && selectedEntries[0].isDir === false) {
      download(selectedEntries[0]);
      return;
    }
    if (selectedEntries.length === 0) {
      makeToast({ kind: "error", message: t($lang, "file.toastNothing") });
      return;
    }
    const name = archiveNameFor(selectedEntries);
    const paths = selectedEntries.map((e) => join(path, e.name));
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

  function deleteSelected() {
    if (!srocket || viewShellId === null) return;
    if (selected.size === 0) return;
    deleteTarget = [...selected];
  }

  function confirmDelete() {
    if (!deleteTarget || !srocket || viewShellId === null) return;
    const names = deleteTarget;
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

  function mkdir() {
    openPrompt("dir");
  }

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

  function uploadFiles(files: File[]) {
    uploadIntoDir(files);
  }

  function uploadFolder(files: File[]) {
    uploadIntoDir(files, true);
  }

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

  async function uploadDropped(
    payload: DropPayload,
    targetDir: string,
  ): Promise<void> {
    const dropped = await collectDropFiles(payload);
    for (const { file, relPath } of dropped) {
      upload(file, join(targetDir, relPath), relPath);
    }
  }

  function folderPicker(node: HTMLInputElement) {
    (node as unknown as { webkitdirectory: boolean }).webkitdirectory = true;
    return {};
  }

  export function handleMessage(message: WsServer) {
    if (message.sftpList) {
      const [listShell, listPath, list, truncated] = message.sftpList;
      if (listShell === viewShellId && listPath === path) {
        applyListing(list, truncated, listPath);
      } else if (
        pendingList &&
        listShell === viewShellId &&
        listPath === pendingList.path
      ) {
        resolvePendingList(list);
      }
    } else if (message.sftpOk) {
      applyAck(message.sftpOk);
    } else if (message.sftpWriteOk) {
      applyWriteAck(message.sftpWriteOk);
    } else if (message.sftpUploadOk) {
      const [savedShell, savedPath, savedSize] = message.sftpUploadOk;
      if (!onUploadVerified(savedShell, savedPath, savedSize)) {
        // Unknown to the upload bookkeeping — an upload started in another
        // FileManager (or one already gone); just show where it landed.
        if (savedShell === viewShellId) refresh();
      }
    } else if (message.sftpCopyProgress) {
      const [, copyFrom, copyBytes] = message.sftpCopyProgress;
      copyBytesByFrom.set(copyFrom, copyBytes);
    } else if (message.error) {
      applyError(message.error);
    }
  }

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

  function applyAck([savedShell, savedPath]: [number, string]) {
    if (pendingDeletes.has(savedPath)) {
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
        copyBytesByFrom.delete(savedPath);
        if (batch.remaining.size === 0) {
          moveCopyBatch = null;
          copyBytesByFrom = new Map();
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

  function applyWriteAck([savedShell, savedPath, offset]: [
    number,
    string,
    number,
  ]) {
    onUploadAck(savedShell, savedPath, offset);
  }

  function applyError(message: string) {
    resolvePendingList();
    if (message.startsWith("删除失败")) {
      pendingDeletes = new Set();
      pendingDeleteNames = [];
    }
    makeToast({ kind: "error", message });
    loading = false;
    onUploadError(message);
    if (message.startsWith("重命名失败") || message.startsWith("复制失败")) {
      moveCopyBatch = null;
      copyBytesByFrom = new Map();
    }
  }

  onMount(() => {
    refresh();
  });

  let hoverTimer: ReturnType<typeof setTimeout> | null = null;
  let hoverTarget: WsSftpEntry | null = null;

  function showHover(event: MouseEvent, entry: WsSftpEntry) {
    hoverTarget = entry;
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

  function onSidebarDragOver(event: DragEvent) {
    if (event.dataTransfer?.types?.includes("Files")) {
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
    }
  }

  function onSidebarDrop(event: DragEvent) {
    if (event.dataTransfer?.files?.length) {
      event.preventDefault();
      const payload = readDropPayload(event.dataTransfer);
      void uploadDropped(payload, path);
    }
  }
</script>

<Sidebar resize={sidebarResize} {open} showHandle={open}>
  <!-- Path bar + refresh -->
  <div class="flex h-9 items-center gap-1 border-b border-zinc-800 px-3">
    <button
      class="shrink-0 text-zinc-400 transition-colors hover:text-zinc-200"
      onclick={refresh}
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
        onblur={commitPath}
      />
    {:else}
      <button
        class="ml-1 flex-1 truncate text-left font-mono text-xs text-zinc-300 hover:text-zinc-100"
        onclick={startEditPath}
        title={t($lang, "file.titleEditPath")}
      >
        {path}
      </button>
    {/if}
  </div>

  {#if moveCopyBatch?.kind === "copy" && copyBytesTotal > 0}
    <div
      class="flex h-6 items-center gap-2 border-b border-indigo-900/40 bg-indigo-900/10 px-3 text-[11px] text-indigo-300"
    >
      <span>{t($lang, "file.copyProgressLabel")}</span>
      <span class="ml-auto font-mono">{formatSize(copyBytesTotal)}</span>
    </div>
  {/if}

  <!-- Search row -->
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
      onkeydown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          searchQuery = "";
        }
      }}
    />
    {#if searchQuery}
      <button
        class="shrink-0 text-zinc-500 transition-colors hover:text-zinc-200"
        onclick={() => (searchQuery = "")}
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
    class="fm-list no-scrollbar flex-1 overflow-y-auto outline-none transition-opacity duration-150"
    class:opacity-60={loading}
    bind:this={listEl}
    tabindex="0"
    role="listbox"
    aria-label={t($lang, "file.list")}
    onkeydown={onKeydown}
    onclick={onListClick}
    onmousedown={onMouseNav}
    onmouseup={onMouseNav}
    onauxclick={onMouseNav}
    ondragover={onSidebarDragOver}
    ondrop={onSidebarDrop}
    oncontextmenu={(event) => {
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
    {#if path !== "/"}
      <div
        class="flex cursor-pointer select-none items-center gap-2 px-3 py-1.5 text-sm text-zinc-400 transition-colors hover:bg-zinc-800 {dropUpActive
          ? 'bg-indigo-600/40 shadow-[inset_3px_0_0_0_#818cf8]'
          : ''}"
        role="button"
        tabindex="-1"
        ondblclick={goUp}
        onkeydown={noop}
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
        onkeydown={noop}
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
        onclick={(event) => {
          if (event.ctrlKey || event.metaKey) toggleSelect(entry.name);
          else if (event.shiftKey) rangeSelect(entry.name);
          else selectOnly(entry.name);
        }}
        oncontextmenu={(event) => {
          event.preventDefault();
          event.stopPropagation();
          if (event.ctrlKey || event.metaKey) toggleSelect(entry.name);
          else if (event.shiftKey) rangeSelect(entry.name);
          else if (!isSelected(entry.name)) selectOnly(entry.name);
          ctxEntry = entry;
          openCtxMenu(event);
        }}
        ondblclick={() => {
          if (entry.isDir) enterDir(entry.name);
          else openEditor(entry);
        }}
        onmousemove={(e) => showHover(e, entry)}
        onmouseleave={hideHover}
      >
        {#if entry.isDir}
          <FolderIcon size="16" class="shrink-0 text-amber-400" />
        {:else}
          {@const Icon = type.icon}
          <Icon size="16" class={`shrink-0 ${type.color}`} />
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
            onclick={(event) => event.stopPropagation()}
            onblur={commitRename}
          />
        {:else}
          <span
            class="min-w-0 flex-1 truncate"
            class:text-sky-300={entry.isLink}
          >
            {entry.name}
          </span>
        {/if}

        <span
          class="fm-mode w-16 shrink-0 ml-1.5 font-mono text-[10px] text-zinc-600"
        >
          {formatMode(entry.mode)}
        </span>

        <span
          class="fm-size w-20 shrink-0 text-right text-xs text-zinc-500"
        >
          {entry.isDir ? "" : formatSize(entry.size)}
        </span>
      </div>
    {/each}
  </div>

  {#if hoverEntry}
    <FileTooltip entry={hoverEntry} x={hoverX} y={hoverY} />
  {/if}

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

  <UploadPanel />
</Sidebar>

<input
  type="file"
  multiple
  class="hidden"
  bind:this={fileInput}
  onchange={handleFilesChange}
/>
<input
  type="file"
  multiple
  class="hidden"
  bind:this={folderInput}
  use:folderPicker
  onchange={handleFolderChange}
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

<ConfirmDialog
  open={overwriteDialog !== null}
  title={t($lang, "file.overwriteTitle")}
  message={overwriteMessage}
  middleText={t($lang, "file.skipBtn")}
  confirmText={t($lang, "file.overwriteBtn")}
  on:confirm={() => {
    if (overwriteDialog && clipboard) {
      doPaste(overwriteDialog.targetDir, clipboard.paths);
    }
    overwriteDialog = null;
  }}
  on:middle={() => {
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

<svelte:window onclick={closeCtxMenu} />

<style lang="postcss">
  /* The list — not the <aside> — carries the container: `container-type`
     implies layout containment, which would make the sidebar the containing
     block for the viewport-fixed FileTooltip rendered as its sibling. */
  .fm-list {
    container-type: inline-size;
  }

  /* Rows drop their fixed columns in reverse priority order as the sidebar
     narrows, so the name — the only identifying column — always keeps room:
     mode goes first (near-constant, and the icon already tells dir from file;
     links are tinted), then size (still in the hover tooltip). Thresholds are
     the widths at which the name would otherwise fall under ~110px. */
  @container (max-width: 330px) {
    .fm-mode {
      display: none;
    }
  }

  @container (max-width: 250px) {
    .fm-size {
      display: none;
    }
  }
</style>
