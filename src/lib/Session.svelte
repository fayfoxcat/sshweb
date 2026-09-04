<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import {
    FolderIcon,
    PlusIcon,
    ServerIcon,
    SettingsIcon,
  } from "svelte-feather-icons";

  import { get } from "svelte/store";

  import { createLock } from "./lock";
  import { fetchAuthStatus } from "./auth";
  import type { WsClient, WsServer, WsServerConfig } from "./protocol";
  import { servers, serverTargetKey, toWsServerConfig } from "./connections";
  import { settings } from "./settings";
  import { Srocket } from "./srocket";
  import { lang, t } from "./i18n";
  import { makeToast } from "./toast";
  import { basename } from "./path";
  import {
    AUTH_PROBE_THROTTLE_MS,
    MAX_TERMINALS,
    PANEL_STORAGE_KEY,
    SESSION_STORAGE_KEY,
  } from "./constants";
  import { createEditors, editorKey, loadEditorState } from "./session/editors";
  import { readSftpView } from "./sftpView";
  import { createSessionRuntime } from "./session/runtime";
  import { createStatsPolling } from "./session/stats";
  import { storageGet, storageSet } from "./storage";
  import { uuid } from "./uuid";
  import themes from "./ui/themes";
  import { createReorderDnd, droppable } from "./ui/dnd";
  import { collectDropFiles, startUpload, type DropPayload } from "./upload";
  import Tab from "./ui/Tab.svelte";
  import XTerm from "./ui/XTerm.svelte";
  import Editor from "./ui/Editor.svelte";
  import FileManager from "./ui/FileManager.svelte";
  import Servers from "./ui/Servers.svelte";
  import ServerStats from "./ui/ServerStats.svelte";
  import Settings from "./ui/Settings.svelte";
  /** Reused UTF-8 decoder for terminal chunks (a fresh `TextDecoder` per
   *  chunk would allocate needlessly on every output burst). */
  const utf8Decoder = new TextDecoder();

  let srocket: Srocket<WsServer, WsClient> | null = null;

  /** Server session key (from the `hello` message); used to build HTTP
   *  download URLs for the file manager. */
  let sessionName = "";

  let connected = false;
  /** False while the initial/reconnect replay is still settling, so the file
   *  manager doesn't follow the active server (or open new servers) until the
   *  restored view has been applied. */
  let replaySettled = false;
  let exitReason: string | null = null;
  let authProbeAt = 0;
  let authProbe: Promise<void> | null = null;
  let settingsOpen = false; // @hmr:keep
  /** Which side panel is open: "servers", "files", or none. The two panels are
   *  mutually exclusive, so every toggle funnels through this single state and
   *  exclusivity is enforced in exactly one place. */
  let openPanel: "servers" | "files" | null = storageGet<
    "servers" | "files" | null
  >(PANEL_STORAGE_KEY, null, (raw) => {
    const v = JSON.parse(raw) as string;
    return v === "servers" || v === "files" ? v : null;
  }); // @hmr:keep
  $: serversOpen = openPanel === "servers";
  $: fileManagerOpen = openPanel === "files";
  /** Persist which panel is open so a refresh restores it. */
  $: {
    storageSet(PANEL_STORAGE_KEY, openPanel);
  }
  let fileManager: any;

  // ---- Terminal shell registry + WS message router -----------------------
  /** Bound "write" method for each terminal. */
  const writers: Record<number, (data: string) => void> = {};
  const locks: Record<number, any> = {};

  /** Shell registry / message router (state lives in a store; see
   *  `$lib/session/runtime.ts`). */
  const rt = createSessionRuntime({
    send: (msg) => srocket?.send(msg),
    toast: (kind, message) => makeToast({ kind, message }),
    onHello: (name) => {
      // Server's real session key, used to build HTTP download URLs.
      sessionName = name;
      exitReason = null;
    },
    onChunks: (shellId, chunks) => {
      locks[shellId](async () => {
        await tick();
        for (const data of chunks) {
          writers[shellId](utf8Decoder.decode(data));
        }
      });
    },
    onFileMessage: (message) => fileManager?.handleMessage(message),
    onPwd: (shellId, path) => {
      // A shell's `pwd` reply: if we were awaiting it for a dropped-file
      // upload, start uploading into that directory now.
      const pending = pendingPwdUploads.get(shellId);
      if (pending && path) {
        pendingPwdUploads.delete(shellId);
        void uploadToTerminal(shellId, path, pending);
      } else {
        pendingPwdUploads.delete(shellId);
      }
    },
    routeEditorData: (sid, path, data) => {
      const key = editorKey(sid, path);
      if (pendingReads.has(key) && openEditors.includes(key)) {
        editorRefs[key]?.loadFile(data);
        pendingReads.delete(key);
        return true;
      }
      return false;
    },
    routeEditorSave: (sid, path) => {
      // The server acknowledged a whole-file save; reset the diff baseline and
      // clear the "modified" indicator (the tab's dirty dot goes away too via
      // `onEditedChange`). Only when the ack matches the editor's shell.
      const key = editorKey(sid, path);
      if (editorShellForKey(key) === sid && editorPathForKey(key) === path) {
        editorRefs[key]?.markSaved();
      }
    },
    openFileBrowser: (sid, cwd, key, explicit) =>
      fileManager?.browseShell(sid, cwd, key, explicit),
    onShellsListed: () => {
      for (const [shellId] of $rtState.shells) {
        locks[shellId] ??= createLock();
      }
    },
    onReplayComplete: async () => {
      // Replay finished: shell ids and the active tab are stable — restore
      // editor tabs and the file-manager view. `tick` lets the reactive
      // `shells`/`activeId` locals catch up with the just-updated store first.
      await tick();
      restoreEditors();
      restoreSftpView();
      // Only after the restored view is in place may the file manager start
      // following the active server (and opening never-opened servers).
      replaySettled = true;
    },
  });
  const rtState = rt.state;
  $: ({ shells, activeId, baseTitles, shellServers, headlessShells } =
    $rtState);

  // ---- File editor (multi-tab) ------------------------------------------
  const editors = createEditors();
  const editorsActive = editors.active;
  $: ({
    open: openEditors,
    pathByKey: editorPaths,
    shellByKey: editorShells,
    minimized: minimizedPaths,
    dirty: dirtyPaths,
  } = $editors);
  $: activeEditorPath = $editorsActive;
  /** Editor component refs, keyed by composite `sid:path` key. */
  const editorRefs: Record<string, Editor> = {};
  /** Composite keys with an `sftpRead` in flight; the matching `sftpData`
   *  reply is routed to the editor. Keyed so two rapid opens don't clobber
   *  each other (a single boolean mis-routed the second reply as a download). */
  const pendingReads: Set<string> = new Set();

  /** The shell an editor belongs to (from the editors store), or -1. */
  function editorShellForKey(key: string): number {
    return editorShells[key] ?? -1;
  }

  /** The path component of a composite editor key. */
  function editorPathForKey(key: string): string {
    return editorPaths[key] ?? "";
  }

  /** Queue an `sftpRead` for `sid:path`, remembering it in `pendingReads` so
   *  the matching `sftpData` reply is routed to the right editor. */
  function sendRead(sid: number, path: string) {
    if (!srocket || sid < 0) return;
    pendingReads.add(editorKey(sid, path));
    srocket.send({ sftpRead: [sid, path] });
  }

  function openEditorByPath(filePath: string, sid: number) {
    if (!srocket) return;
    editors.open(filePath, sid);
    // Re-read fresh content each time the editor is opened.
    sendRead(sid, filePath);
  }

  /** Re-read a file from disk into its editor (the 还原 button reloads the
   *  latest server content, which may differ from what was loaded on open). */
  function reloadEditor(key: string) {
    sendRead(editorShellForKey(key), editorPathForKey(key));
  }

  function activateEditor(key: string) {
    editors.activate(key);
  }

  function minimizeEditor(key: string) {
    editors.minimize(key);
  }

  function closeEditor(key: string) {
    editorRefs[key]?.markClosed();
    editors.close(key);
    delete editorRefs[key];
  }

  let editorsRestored = false;
  /** Re-read the content of each restored editor tab after a refresh. The tab
   *  bar itself is already seeded from sessionStorage (see `createEditors`);
   *  this sends the `sftpRead` so the Editor loads fresh disk content and then
   *  overlays its unsaved draft (nothing typed before the refresh is lost). */
  function restoreEditors() {
    if (editorsRestored) return;
    editorsRestored = true;
    const saved = loadEditorState();
    if (!saved || !srocket) return;
    for (const key of saved.open) {
      // Re-read from the shell each editor was opened on (not the current
      // active terminal), so a file on server A isn't read from server B.
      const path = saved.pathByKey[key] ?? "";
      const sid = saved.shellByKey[key] ?? activeId;
      if (sid < 0 || !path) continue;
      sendRead(sid, path);
    }
  }

  let viewRestored = false;
  /** Restore the file-manager view (browsed shell + path) after a refresh, if
   *  the target shell still exists. Only on the first replay (a transient
   *  reconnect keeps the already-correct view). */
  function restoreSftpView() {
    if (viewRestored) return;
    viewRestored = true;
    const saved = readSftpView();
    if (!saved || !fileManager) return;
    const valid =
      shells.some(([sid]) => sid === saved.viewShellId) ||
      saved.viewShellId in $rtState.headlessShells;
    if (valid) {
      const server =
        $rtState.headlessShells[saved.viewShellId] ??
        $rtState.shellServers[saved.viewShellId] ??
        null;
      fileManager.applyRestoredView(
        saved.path,
        saved.viewShellId,
        serverTargetKey(server),
      );
    }
  }

  /** Connection status: green when the transport is up and a terminal is open. */
  $: linkOk = connected && shells.length > 0;

  /** Keep the server's notion of the active tab in sync (survives a refresh).
   *  Fires whenever the active terminal changes while connected; echoes during
   *  replay are idempotent. */
  $: if (connected && srocket && activeId >= 0) {
    rt.send({ setActive: activeId });
  }

  /** Per-shell system stats; only the active shell is polled. */
  const statsPolling = createStatsPolling();
  const statsByShellStore = statsPolling.byShell;

  $: themeBg = themes[$settings.theme].background;

  /** Restart the stats poller whenever the active tab / connection changes. */
  function restartStats() {
    statsPolling.restart({
      sessionName: () => sessionName,
      shellServers: () => shellServers,
      activeId: () => activeId,
      connected: () => connected,
    });
  }

  /**
   * A server restart invalidates the in-memory auth session. Browsers keep the
   * old page alive in that case, so the WebSocket receives a 401 while
   * Srocket's reconnect loop otherwise has no visible error. Refresh auth
   * state after disconnecting so AuthGate can show the login form again.
   * Throttle the probe to avoid polling the endpoint during a real network
   * outage.
   */
  function probeAuthAfterDisconnect() {
    const now = Date.now();
    if (authProbe || now - authProbeAt < AUTH_PROBE_THROTTLE_MS) return;
    authProbeAt = now;
    authProbe = fetchAuthStatus()
      .then(() => undefined)
      .catch(() => undefined)
      .finally(() => {
        authProbe = null;
      });
  }

  // Restart the stats timer whenever the active tab changes.
  $: activeId, connected, restartStats();
  /** Drop stats of closed shells so a long session doesn't accumulate stale
   *  records (only the active shell is ever polled). */
  $: {
    statsPolling.prune(shells.map(([sid]) => sid));
  }

  /** Display title for a tab. Duplicate base names are numbered by **creation
   *  order (sid)**, never by the current tab position — so drag-reordering the
   *  tabs doesn't renumber the titles (each shell keeps its identity), and the
   *  numbering survives a refresh (sids are stable per session). */
  function tabTitle(shellId: number): string {
    const base = baseTitles[shellId] ?? t($lang, "session.tabDefault");
    const same = shells
      .filter(
        ([sid]) => (baseTitles[sid] ?? t($lang, "session.tabDefault")) === base,
      )
      .sort(([a], [b]) => a - b);
    if (same.length === 1) return base;
    const idx = same.findIndex(([sid]) => sid === shellId) + 1;
    return `${base} ${idx}`;
  }

  onMount(() => {
    // Stable per-browser session key: reconnect to the same server-side
    // session after a refresh, so terminals/processes are preserved.
    let sessionKey = sessionStorage.getItem(SESSION_STORAGE_KEY);
    if (!sessionKey) {
      // `uuid()` falls back for plain-http (non-secure) contexts where
      // `crypto.randomUUID` is unavailable.
      sessionKey = uuid();
      sessionStorage.setItem(SESSION_STORAGE_KEY, sessionKey);
    }
    srocket = new Srocket<WsServer, WsClient>(`/api/s/${sessionKey}`, {
      onMessage(message) {
        rt.dispatch(message);
      },

      onConnect() {
        connected = true;
      },

      onDisconnect() {
        connected = false;
        replaySettled = false;
        probeAuthAfterDisconnect();
        // Do NOT reset the shell registry: the session keeps running on the
        // server (output buffered), and on reconnect the server replays the
        // full state (hello/shells/labels/buffer). Clear the terminal
        // screens so the replay doesn't duplicate what was already shown.
        for (const write of Object.values(writers)) {
          if (write) write("\x1b[2J\x1b[3J\x1b[H");
        }
        pendingReads.clear();
        statsPolling.stop();
      },

      onClose(event) {
        if (event.code === 4404) {
          exitReason = t($lang, "session.connFailed") + event.reason;
        } else if (event.code === 4500) {
          exitReason = t($lang, "session.serverError") + event.reason;
        }
      },
    });
  });

  onDestroy(() => {
    srocket?.dispose();
    statsPolling.stop();
  });

  function guardLimit(): boolean {
    if (shells.length >= MAX_TERMINALS) {
      makeToast({
        kind: "error",
        message: t($lang, "session.limit", { n: MAX_TERMINALS }),
      });
      return true;
    }
    return false;
  }

  /** Create a shell named after the given base name. The connect result is
   *  reported by the runtime: a success toast on the first terminal output for
   *  remote shells, or the server's error toast on failure. */
  function createShell(
    name: string,
    server: WsServerConfig | null,
    cwd: string | null = null,
  ) {
    if (guardLimit()) return;
    rt.beginCreate(name, server, cwd);
  }

  /** Open a new terminal starting in the given directory (SSH if the viewed
   *  shell is remote, local otherwise). */
  function openShellInDir(payload: { dir: string; sid: number | null }) {
    const sid = payload.sid ?? activeId;
    let server: WsServerConfig | null = null;
    if (sid >= 0) {
      server = $rtState.headlessShells[sid] ?? null;
      if (!server) server = $rtState.shellServers[sid] ?? null;
    }
    createShell(
      server ? server.name : t($lang, "session.tabDefault"),
      server,
      payload.dir,
    );
  }

  /** 本机卡片「新建本地终端」/ 空态按钮:显式创建本地终端(不跟随激活服务器)。 */
  function createLocalTerminal() {
    createShell(t($lang, "session.tabDefault"), null);
  }

  /** Tab bar "+": 跟随激活终端所在服务器新建终端——远程终端 → 同服务器新终端,本地
   *  终端 → 本地。无激活终端时回退为本地。 */
  function handleCreateFollowingActive() {
    const server = activeId >= 0 ? shellServers[activeId] ?? null : null;
    createShell(server ? server.name : t($lang, "session.tabDefault"), server);
  }

  /** Labels per sid for the file manager header / search hint (user@host). */
  let targetNames: Record<number, string> = {};
  /** Server identity per shell (`user@host:port` or "local"), for the file
   *  manager's per-server view binding: all terminals of one server share a
   *  single SFTP view, so switching terminals keeps the file list bound. */
  let shellServerKeys: Record<number, string> = {};
  /** Compute both sid→label maps in a single pass over the shell registry
   *  (headless shells contribute a label but never a view key). */
  $: {
    const names: Record<number, string> = {};
    const keys: Record<number, string> = {};
    for (const [sid] of shells) {
      const server = shellServers[sid];
      names[sid] = server
        ? t($lang, "session.hostLabel", {
            user: server.username,
            host: server.host,
            name: server.name,
          })
        : t($lang, "session.localShell");
      keys[sid] = serverTargetKey(server ?? null);
    }
    for (const [sid, server] of Object.entries(headlessShells)) {
      names[Number(sid)] = t($lang, "session.hostLabel", {
        user: server.username,
        host: server.host,
        name: server.name,
      });
    }
    targetNames = names;
    shellServerKeys = keys;
  }

  /** Connect a headless SFTP session to a saved server and browse its files.
   *  No terminal tab is opened; the file manager follows the result. */
  async function connectSavedServer(serverId: string) {
    const server = $servers.servers.find((s) => s.id === serverId);
    if (!server) {
      makeToast({ kind: "error", message: t($lang, "session.serverNotFound") });
      return;
    }
    try {
      const config = toWsServerConfig(server);
      rt.setPendingOpen({ fromSid: activeId, server: config, fromList: true });
      rt.send({ sftpConnect: config });
      // No intermediate "connecting" toast: the result is reported by
      // `sftpOpenResult` — a success toast on clean open, or a notice/error
      // toast on fallback or failure.
    } catch (err) {
      console.error(err);
      makeToast({
        kind: "error",
        message: t($lang, "session.connFilesFailed"),
      });
    }
  }

  /** Open SFTP for the active terminal: follow its current directory and
   *  user (e.g. after `su`), then show the file manager sidebar.
   *
   *  The identity probe runs on **every** click, not only the first toggle:
   *  the sidebar may already be open (e.g. browsing files as the login user)
   *  when the terminal switches to root; clicking the button re-binds the
   *  file system to the active terminal's current identity. */
  function toggleFileManager() {
    const opening = openPanel !== "files";
    if (opening) {
      // Show the loading state immediately instead of the previous listing.
      fileManager?.prepareBrowse();
    }
    if (activeId >= 0 && connected && srocket) {
      rt.setPendingOpen({
        fromSid: activeId,
        // For a remote terminal this names its server so the view binds to the
        // server (not the sid); local terminals stay null → "local".
        server: shellServers[activeId] ?? null,
      });
      rt.send({ sftpOpen: activeId });
    }
    openPanel = openPanel === "files" ? null : "files";
  }

  /** Open the active terminal's server in the file manager. Fired when the
   *  file manager (open) follows the active terminal to a server that was
   *  never opened there — the view always follows the active server, so the
   *  first open follows this terminal's known directory. */
  function followActiveServer() {
    if (activeId < 0 || !connected || !srocket) return;
    // Show the loading state immediately while the probe runs.
    fileManager?.prepareBrowse();
    rt.setPendingOpen({
      fromSid: activeId,
      server: shellServers[activeId] ?? null,
      follow: true,
    });
    rt.send({ sftpOpen: activeId });
  }

  /** Open the file manager and connect a saved server's SFTP (folder icon in
   *  the server list). */
  function openSavedServerSftp(serverId: string) {
    // Clear and show the loading state right away: the probe + first listing
    // take at least one SSH handshake, during which the old server's files
    // must never stay visible.
    fileManager?.prepareBrowse();
    connectSavedServer(serverId);
    openPanel = "files";
  }

  /** Open the local machine's file system: browse an existing local shell, or
   *  create one first (the server list's fixed "本机" entry). */
  function openLocalSftp() {
    const local = shells.find(([sid]) => !shellServers[sid]);
    if (local) {
      rt.setPendingOpen({ fromSid: local[0], server: null });
      rt.send({ sftpOpen: local[0] });
    } else {
      rt.setPendingLocalSftp(true);
      createShell(t($lang, "session.tabDefault"), null);
    }
    fileManager?.prepareBrowse();
    openPanel = "files";
  }

  function handleClose(shellId: number) {
    rt.send({ close: shellId });
  }

  // ---- Tab drag-to-sort --------------------------------------------------
  /** Shared drag-reorder state for the tab strip (see `createReorderDnd`). */
  const {
    source: tabsSource,
    over: tabsOver,
    start: tabsDragStart,
    end: tabsDragEnd,
    overTarget: tabsDragOver,
    leave: tabsDragLeave,
    drop: tabsDragDrop,
  } = createReorderDnd<number>();

  /** Reorder so the dragged tab takes the drop target's position, then let the
   *  server apply & rebroadcast (order is server-side, survives a refresh). */
  function onTabDrop(targetId: number) {
    const from = tabsDragDrop();
    if (from === null || from === targetId) return;
    const order = shells.map(([sid]) => sid);
    const fromIdx = order.indexOf(from);
    const toIdx = order.indexOf(targetId);
    if (fromIdx === -1 || toIdx === -1) return;
    order.splice(fromIdx, 1);
    order.splice(order.indexOf(targetId), 0, from);
    rt.send({ reorderShells: order });
  }

  /** Drop anywhere on the tab strip that isn't a tab (a gap or the "+"
   *  button): move the dragged tab to the end. When the drop lands on a tab,
   *  that tab's own `onTabDrop` already handled it (it clears the drag source),
   *  so this early-returns. */
  function onStripDrop(event: DragEvent) {
    const from = get(tabsSource);
    if (from === null) return;
    const el = document.elementFromPoint(event.clientX, event.clientY);
    if (el?.closest?.("[role=tab]")) return; // handled by the tab itself
    tabsDragDrop();
    const order = shells.map(([sid]) => sid);
    const fromIdx = order.indexOf(from);
    if (fromIdx === -1) return;
    order.splice(fromIdx, 1);
    order.push(from);
    rt.send({ reorderShells: order });
  }

  function handleInput(shellId: number, data: Uint8Array) {
    rt.send({ data: [shellId, data] });
  }

  function handleResize(shellId: number, rows: number, cols: number) {
    if (shellId !== activeId) return;
    rt.send({ resize: [shellId, { rows, cols }] });
  }

  // ---- Terminal drop-upload (drag local files/folders onto a terminal) ----
  /** Drop payload awaiting the shell's `pwd` reply before uploading. */
  const pendingPwdUploads = new Map<number, DropPayload>();

  /** Start uploading dropped files/folders into `dir` on `shellId`, walking
   *  the drop payload so folder sub-trees are preserved. */
  async function uploadToTerminal(
    shellId: number,
    dir: string,
    payload: DropPayload,
  ) {
    const server = shellServers[shellId] ?? null;
    const targetName = server
      ? serverTargetKey(server)
      : t($lang, "session.localShell");
    const dropped = await collectDropFiles(payload);
    for (const { file, relPath } of dropped) {
      startUpload({
        file,
        destPath: `${dir}/${relPath}`,
        displayName: relPath,
        targetShell: shellId,
        targetName,
        socket: srocket!,
        onDone: () => {},
      });
    }
  }

  /** Handle files/folders dropped onto a terminal: query its pwd, then upload. */
  function handleTerminalDrop(shellId: number, payload: DropPayload) {
    if (
      !srocket ||
      (payload.entries.length === 0 && payload.files.length === 0)
    )
      return;
    pendingPwdUploads.set(shellId, payload);
    rt.send({ pwdRequest: shellId });
  }
</script>

<main class="flex h-screen flex-col overflow-hidden" style:background={themeBg}>
  <!-- Tab bar -->
  <header
    class="flex items-center gap-1 border-b border-zinc-800 bg-zinc-900 px-2 py-1.5"
  >
    <button
      class="rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
      class:bg-zinc-800={serversOpen}
      class:text-zinc-200={serversOpen}
      on:click={() => {
        openPanel = openPanel === "servers" ? null : "servers";
      }}
      title={t($lang, "session.titleServers")}
    >
      <ServerIcon size="18" />
    </button>
    <button
      class="rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
      class:bg-zinc-800={fileManagerOpen}
      class:text-zinc-200={fileManagerOpen}
      on:click={toggleFileManager}
      title={t($lang, "session.titleFiles")}
    >
      <FolderIcon size="18" />
    </button>
    <button
      class="rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
      on:click={() => (settingsOpen = true)}
      title={t($lang, "session.titleSettings")}
    >
      <SettingsIcon size="18" />
    </button>

    <div class="mx-1 h-5 border-l border-zinc-800" />

    <div
      class="flex flex-1 items-center gap-1 overflow-x-auto no-scrollbar"
      use:droppable={{
        onDragOver: () => $tabsSource !== null,
        onDrop: onStripDrop,
        onDragLeave: () => {},
      }}
    >
      {#each shells as [shellId, winsize] (shellId)}
        <Tab
          variant="terminal"
          active={shellId === activeId}
          title={tabTitle(shellId)}
          closeTitle={t($lang, "session.titleCloseTab")}
          dragKey={String(shellId)}
          dragOver={$tabsOver === shellId}
          onTabDragStart={() => tabsDragStart(shellId)}
          onTabDragEnd={tabsDragEnd}
          onTabDragOver={() => tabsDragOver(shellId)}
          onTabDrop={(key) => onTabDrop(Number(key))}
          onTabDragLeave={tabsDragLeave}
          onActivate={() => rt.setActive(shellId)}
          onClose={() => handleClose(shellId)}
        >
          <span class="truncate">{tabTitle(shellId)}</span>
        </Tab>
      {/each}
      <!-- The new-terminal button follows the rightmost tab (scrolls with
           the tab strip, instead of being pinned to the left toolbar). It
           creates a terminal on the ACTIVE terminal's server. -->
      <button
        class="shrink-0 rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200 disabled:opacity-50"
        on:click={handleCreateFollowingActive}
        disabled={!connected}
        title={t($lang, "session.titleNewTerminal")}
      >
        <PlusIcon size="18" />
      </button>
    </div>

    <div class="ml-auto flex shrink-0 items-center gap-2 pr-1">
      <ServerStats stats={$statsByShellStore[activeId] ?? null} />
      <!-- Connection status: green when a terminal is connected, red when not -->
      <div
        class="h-3.5 w-3.5 shrink-0 rounded-full ring-2 ring-black/40 transition-colors"
        class:bg-emerald-500={linkOk}
        class:bg-red-500={!linkOk}
        style:box-shadow={linkOk
          ? "0 0 9px 2px rgba(16,185,129,0.55)"
          : "0 0 9px 2px rgba(239,68,68,0.55)"}
        title={linkOk
          ? t($lang, "session.connected")
          : t($lang, "session.disconnected")}
      />
    </div>
  </header>

  <Settings open={settingsOpen} on:close={() => (settingsOpen = false)} />

  <!-- Terminal area -->
  <div class="flex flex-1 overflow-hidden">
    {#if serversOpen}
      <Servers
        on:connect={(event) => {
          openPanel = null;
          const server = event.detail;
          createShell(server.name, server);
        }}
        on:openSftp={(event) => {
          openSavedServerSftp(event.detail);
        }}
        on:connectLocal={createLocalTerminal}
        on:openLocalSftp={openLocalSftp}
      />
    {/if}
    <!-- Always mounted so in-progress transfers keep working while hidden. -->
    <FileManager
      bind:this={fileManager}
      {srocket}
      {sessionName}
      shellId={activeId >= 0 ? activeId : null}
      {shellServerKeys}
      {targetNames}
      open={fileManagerOpen}
      {replaySettled}
      on:openEditor={(event) =>
        openEditorByPath(event.detail.path, event.detail.sid)}
      on:sshInDir={(event) => openShellInDir(event.detail)}
      on:followActive={followActiveServer}
    />
    <div class="relative flex-1 overflow-hidden">
      {#if exitReason !== null}
        <div
          class="absolute inset-0 flex items-center justify-center px-6 text-center text-red-400"
        >
          {exitReason}
        </div>
      {:else if shells.length === 0}
        <div
          class="absolute inset-0 flex flex-col items-center justify-center gap-4 text-zinc-400"
        >
          <p>{t($lang, "session.noTerminal")}</p>
          <button
            class="rounded-full bg-pink-700 px-6 py-2 font-medium text-white transition-colors hover:bg-pink-600 disabled:opacity-50"
            on:click={createLocalTerminal}
            disabled={!connected}
          >
            {t($lang, "session.newTerminal")}
          </button>
        </div>
      {:else}
        {#each shells as [shellId, winsize] (shellId)}
          <div
            class="absolute inset-0 p-3"
            style:background={themeBg}
            style:visibility={shellId === activeId ? "visible" : "hidden"}
          >
            <XTerm
              rows={winsize.rows}
              cols={winsize.cols}
              active={shellId === activeId}
              bind:write={writers[shellId]}
              on:data={({ detail: data }) => handleInput(shellId, data)}
              on:resize={({ detail }) =>
                handleResize(shellId, detail.rows, detail.cols)}
              on:dropfiles={({ detail }) =>
                handleTerminalDrop(shellId, detail.payload)}
            />
          </div>
        {/each}
      {/if}
    </div>
  </div>

  <!-- Editor tab bar: only when editors are open; terminal shrinks above it -->
  {#if openEditors.length > 0}
    <div
      class="flex shrink-0 items-center gap-1 overflow-x-auto no-scrollbar border-t border-zinc-800 bg-zinc-900 px-2 py-1"
    >
      {#each openEditors as key (key)}
        {@const path = editorPathForKey(key)}
        {@const name = targetNames[editorShellForKey(key)] ?? ""}
        <Tab
          variant="editor"
          active={key === activeEditorPath}
          title={path}
          closeTitle={t($lang, "editor.closeDirtyTitle")}
          onActivate={() => activateEditor(key)}
          onClose={() => closeEditor(key)}
        >
          <span
            class="h-1 w-1 shrink-0 rounded-full"
            class:bg-amber-400={dirtyPaths.has(key)}
            class:bg-transparent={!dirtyPaths.has(key)}
          />
          <span class="max-w-[110px] truncate">{basename(path)}</span>
          {#if name}
            <span
              class="shrink-0 max-w-[90px] truncate text-[9px] text-zinc-500"
              >{name}</span
            >
          {/if}
        </Tab>
      {/each}
    </div>
  {/if}

  <!-- All open editors are layered; minimized ones are hidden. Tabs keep open
       order; the active (focused) editor is always drawn on top. Each editor
       is bound to the shell it was opened from — switching the active terminal
       never re-targets it to a different server. -->
  {#each openEditors as key, i (key)}
    {@const path = editorPathForKey(key)}
    {@const sid = editorShellForKey(key)}
    <Editor
      bind:this={editorRefs[key]}
      {srocket}
      shellId={sid >= 0 ? sid : null}
      filePath={path}
      active={!minimizedPaths.has(key)}
      zIndex={key === activeEditorPath ? 60 + openEditors.length : 60 + i}
      onMinimize={() => minimizeEditor(key)}
      onActivate={() => activateEditor(key)}
      onEditedChange={(dirty) => editors.markDirty(key, dirty)}
      onReload={() => reloadEditor(key)}
      onClose={() => closeEditor(key)}
    />
  {/each}
</main>
