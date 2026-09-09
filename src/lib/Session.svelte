<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { FolderIcon, PlusIcon, ServerIcon, SettingsIcon } from "$lib/icons";

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
  /** Reused UTF-8 decoder for terminal chunks. */
  const utf8Decoder = new TextDecoder();

  let srocket = $state<Srocket<WsServer, WsClient> | null>(null);

  let sessionName = $state("");

  let connected = $state(false);
  let replaySettled = $state(false);
  let exitReason = $state<string | null>(null);
  let authProbeAt = $state(0);
  let authProbe: Promise<void> | null = null;
  let settingsOpen = $state(false);
  let openPanel = $state<"servers" | "files" | null>(
    storageGet<"servers" | "files" | null>(PANEL_STORAGE_KEY, null, (raw) => {
      const v = JSON.parse(raw) as string;
      return v === "servers" || v === "files" ? v : null;
    }),
  );

  let serversOpen = $derived(openPanel === "servers");
  let fileManagerOpen = $derived(openPanel === "files");

  $effect(() => {
    storageSet(PANEL_STORAGE_KEY, openPanel);
  });

  let fileManager: any;

  const writers: Record<number, (data: string) => void> = {};
  const locks: Record<number, any> = {};

  const rt = createSessionRuntime({
    send: (msg) => srocket?.send(msg),
    toast: (kind, message) => makeToast({ kind, message }),
    onHello: (name) => {
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
      await tick();
      restoreEditors();
      restoreSftpView();
      replaySettled = true;
    },
  });

  const rtState = rt.state;
  let shells = $derived($rtState.shells);
  let activeId = $derived($rtState.activeId);
  let baseTitles = $derived($rtState.baseTitles);
  let numbers = $derived($rtState.numbers);
  let shellServers = $derived($rtState.shellServers);
  let headlessShells = $derived($rtState.headlessShells);

  // ---- File editor (multi-tab) ------------------------------------------
  const editors = createEditors();
  const editorsActive = editors.active;
  let openEditors = $derived($editors.open);
  let editorPaths = $derived($editors.pathByKey);
  let editorShells = $derived($editors.shellByKey);
  let minimizedPaths = $derived($editors.minimized);
  let dirtyPaths = $derived($editors.dirty);
  let activeEditorPath = $derived($editorsActive);

  const editorRefs: Record<string, Editor> = {};
  const pendingReads: Set<string> = new Set();

  function editorShellForKey(key: string): number {
    return editorShells[key] ?? -1;
  }

  function editorPathForKey(key: string): string {
    return editorPaths[key] ?? "";
  }

  function sendRead(sid: number, path: string) {
    if (!srocket || sid < 0) return;
    pendingReads.add(editorKey(sid, path));
    srocket.send({ sftpRead: [sid, path] });
  }

  function openEditorByPath(filePath: string, sid: number) {
    if (!srocket) return;
    editors.open(filePath, sid);
    sendRead(sid, filePath);
  }

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
  function restoreEditors() {
    if (editorsRestored) return;
    editorsRestored = true;
    const saved = loadEditorState();
    if (!saved || !srocket) return;
    for (const key of saved.open) {
      const path = saved.pathByKey[key] ?? "";
      const sid = saved.shellByKey[key] ?? activeId;
      if (sid < 0 || !path) continue;
      sendRead(sid, path);
    }
  }

  let viewRestored = false;
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

  let linkOk = $derived(connected && shells.length > 0);

  $effect(() => {
    if (connected && srocket && activeId >= 0) {
      rt.send({ setActive: activeId });
    }
  });

  const statsPolling = createStatsPolling();
  const statsByShellStore = statsPolling.byShell;

  let themeBg = $derived(themes[$settings.theme].background);

  function restartStats() {
    statsPolling.restart({
      sessionName: () => sessionName,
      shellServers: () => shellServers,
      activeId: () => activeId,
      connected: () => connected,
    });
  }

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

  $effect(() => {
    restartStats();
  });

  $effect(() => {
    statsPolling.prune(shells.map(([sid]) => sid));
  });

  function tabTitle(shellId: number): string {
    const base = baseTitles[shellId] ?? t($lang, "session.tabDefault");
    const same = shells
      .filter(
        ([sid]) => (baseTitles[sid] ?? t($lang, "session.tabDefault")) === base,
      )
      .sort(([a], [b]) => a - b);
    if (same.length === 1) return base;
    const n = numbers[shellId];
    if (n) return `${base} ${n}`;
    const idx = same.findIndex(([sid]) => sid === shellId) + 1;
    return `${base} ${idx}`;
  }

  onMount(() => {
    let sessionKey = sessionStorage.getItem(SESSION_STORAGE_KEY);
    if (!sessionKey) {
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

  function createShell(
    name: string,
    server: WsServerConfig | null,
    cwd: string | null = null,
  ) {
    if (guardLimit()) return;
    let dir = cwd;
    if (dir === null && server) {
      const startupDir = (server.startupDir ?? "").trim();
      dir =
        startupDir === "" || startupDir === "~" || startupDir === "~/"
          ? null
          : startupDir;
    }
    rt.beginCreate(name, server, dir);
  }

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

  function createLocalTerminal() {
    createShell(t($lang, "session.tabDefault"), null);
  }

  function handleCreateFollowingActive() {
    const server = activeId >= 0 ? (shellServers[activeId] ?? null) : null;
    createShell(server ? server.name : t($lang, "session.tabDefault"), server);
  }

  let targetNames = $state<Record<number, string>>({});
  let shellServerKeys = $state<Record<number, string>>({});

  $effect(() => {
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
  });

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
    } catch (err) {
      console.error(err);
      makeToast({
        kind: "error",
        message: t($lang, "session.connFilesFailed"),
      });
    }
  }

  function toggleFileManager() {
    const opening = openPanel !== "files";
    if (opening) {
      fileManager?.prepareBrowse();
    }
    if (activeId >= 0 && connected && srocket) {
      rt.setPendingOpen({
        fromSid: activeId,
        server: shellServers[activeId] ?? null,
      });
      rt.send({ sftpOpen: activeId });
    }
    openPanel = openPanel === "files" ? null : "files";
  }

  function followActiveServer() {
    if (activeId < 0 || !connected || !srocket) return;
    fileManager?.prepareBrowse();
    rt.setPendingOpen({
      fromSid: activeId,
      server: shellServers[activeId] ?? null,
      follow: true,
    });
    rt.send({ sftpOpen: activeId });
  }

  function openSavedServerSftp(serverId: string) {
    fileManager?.prepareBrowse();
    connectSavedServer(serverId);
    openPanel = "files";
  }

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
  const {
    source: tabsSource,
    over: tabsOver,
    start: tabsDragStart,
    end: tabsDragEnd,
    overTarget: tabsDragOver,
    leave: tabsDragLeave,
    drop: tabsDragDrop,
  } = createReorderDnd<number>();

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

  function onStripDrop(event: DragEvent) {
    const from = get(tabsSource);
    if (from === null) return;
    const el = document.elementFromPoint(event.clientX, event.clientY);
    if (el?.closest?.("[role=tab]")) return;
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

  // ---- Terminal drop-upload ----------------------------------------------
  const pendingPwdUploads = new Map<number, DropPayload>();

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
      onclick={() => {
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
      onclick={toggleFileManager}
      title={t($lang, "session.titleFiles")}
    >
      <FolderIcon size="18" />
    </button>
    <button
      class="rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200"
      onclick={() => (settingsOpen = true)}
      title={t($lang, "session.titleSettings")}
    >
      <SettingsIcon size="18" />
    </button>

    <div class="mx-1 h-5 border-l border-zinc-800"></div>

    <div
      class="flex flex-1 items-center gap-1 overflow-x-auto no-scrollbar"
      use:droppable={{
        onDragOver: () => $tabsSource !== null,
        onDrop: onStripDrop,
        onDragLeave: () => {},
      }}
    >
      {#each shells as [shellId] (shellId)}
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
      <button
        class="shrink-0 rounded-md p-1.5 text-zinc-400 transition-colors hover:bg-zinc-800 hover:text-zinc-200 disabled:opacity-50"
        onclick={handleCreateFollowingActive}
        disabled={!connected}
        title={t($lang, "session.titleNewTerminal")}
      >
        <PlusIcon size="18" />
      </button>
    </div>

    <div class="ml-auto flex shrink-0 items-center gap-2 pr-1">
      <ServerStats stats={$statsByShellStore[activeId] ?? null} />
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
      ></div>
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
            onclick={createLocalTerminal}
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

  <!-- Editor tab bar -->
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
          ></span>
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

  <!-- Layered editors -->
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
