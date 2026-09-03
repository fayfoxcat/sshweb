import { get, writable } from "svelte/store";

import { servers, serverTargetKey } from "../connections";
import { tr } from "../i18n";
import type { Toast } from "../toast";
import type {
  WsClient,
  WsServer,
  WsServerConfig,
  WsWinsize,
} from "../protocol";

/** Terminal-tab registry state for one session (data only: the component
 *  keeps writers/locks/editor refs, which are DOM-bound). */
type ShellRuntimeState = {
  /** Open visible shells, in display order (frontmost last). */
  shells: [number, WsWinsize][];
  /** Active (visible) terminal tab. */
  activeId: number;
  /** Base display name (without dedup suffix) for each shell. */
  baseTitles: Record<number, string>;
  /** Server config used by each shell ("open SSH in directory" identity). */
  shellServers: Record<number, WsServerConfig | null>;
  /** Headless SFTP shells (no terminal tab) keyed by sid. */
  headlessShells: Record<number, WsServerConfig>;
  /** Names to assign to shells created by pending `create` requests. */
  pendingNames: string[];
  /** Server configs to assign to shells created by pending `create` requests. */
  pendingServers: (WsServerConfig | null)[];
};

const initial = (): ShellRuntimeState => ({
  shells: [],
  activeId: -1,
  baseTitles: {},
  shellServers: {},
  headlessShells: {},
  pendingNames: [],
  pendingServers: [],
});

/** The in-flight SFTP open request. `server` is set for saved-server requests
 *  (`sftpConnect`) and for terminal requests whose terminal is remote (so the
 *  file-manager view can bind to the server, not the sid); it is null for
 *  local terminals. `fromList` marks the server-list SFTP button — the only
 *  source of the "已打开 X 的文件系统" success toast. `follow` marks a
 *  follow-the-active-terminal open (its result is dropped if the active
 *  terminal has since switched servers). */
type SftpOpenFlow = {
  fromSid: number;
  server: WsServerConfig | null;
  fromList?: boolean;
  follow?: boolean;
};

/** The server redacts SSH passwords from the session replay (安全审查 M6). The
 *  browser keeps the full config it submitted; merge the replayed (redacted)
 *  config with the saved one by target so opening a new terminal on the same
 *  server still authenticates. */
function mergeReplayConfig(cfg: WsServerConfig): WsServerConfig {
  const saved = get(servers).servers.find(
    (s) => serverTargetKey(s) === serverTargetKey(cfg),
  );
  if (!saved) return cfg;
  return {
    ...cfg,
    password: saved.password || cfg.password,
    hosts: saved.hosts ?? cfg.hosts,
    proxy: saved.proxy ?? cfg.proxy,
  };
}

/** Glue the component's DOM/Browser-bound capabilities into the runtime. */
type RuntimeEnv = {
  /** Send a client message over the WebSocket (no-op when disconnected). */
  send: (msg: WsClient) => void;
  toast: (kind: Toast["kind"], message: string) => void;
  /** `hello` arrived: record the session key (HTTP URLs depend on it). */
  onHello: (sessionName: string) => void;
  /** Terminal output for a shell (routed through per-shell write locks). */
  onChunks: (shellId: number, chunks: Uint8Array[]) => void;
  /** Forward a bundle message to the file manager sidebar. */
  onFileMessage: (message: WsServer) => void;
  /** A shell reported its current working directory (`pwd` reply). */
  onPwd: (shellId: number, path: string) => void;
  /** Editor-read interception: true when the `sftpData` was consumed. `sid` is
   *  the shell the editor belongs to (a path may exist on several servers). */
  routeEditorData: (sid: number, path: string, data: Uint8Array) => boolean;
  /** A save (`sftpWrite`) was acknowledged: reset the editor's diff baseline
   *  and clear its "modified" indicator for `sid:path`. */
  routeEditorSave: (sid: number, path: string) => void;
  /** A browse target resolved: open/rebind the file manager. `key` is the
   *  server identity (`user@host:port` or "local") the view is bound to.
   *  `explicit` is true only for server-list opens (a specific server target);
   *  terminal / follow opens pass false so a stale result (the active terminal
   *  moved on while the probe was in flight) is discarded. */
  openFileBrowser: (
    sid: number,
    cwd: string,
    key: string,
    explicit: boolean,
  ) => void;
  /** A new shell list arrived (create per-shell write locks here). */
  onShellsListed: () => void;
  /** The reconnect replay has finished (the last state message `activeShell`
   *  was applied): shell ids / active tab are stable, safe to restore
   *  editor tabs and the file-manager view. */
  onReplayComplete: () => void;
};

/** Terminal-registry + message router for the session WebSocket.
 *
 *  Shell creation naming, active-tab selection, headless SFTP bookkeeping and
 *  the editor-read interception all flow through `dispatch()`, keeping the
 *  component down to DOM glue (writers/locks/stats/panels). */
export function createSessionRuntime(env: RuntimeEnv) {
  const state = writable<ShellRuntimeState>(initial());
  let prevIds: number[] = [];
  let pendingSftpOpen: SftpOpenFlow | null = null;
  let pendingLocalSftp = false;
  /** True while the reconnect replay has not finished; `onReplayComplete` is
   *  fired once per attach when the last state message is applied. */
  let replayDone = true;
  /** Remote-server shells awaiting their first terminal output: sid → server
   *  name. A success toast fires once the shell produces output (i.e. the SSH
   *  connection is really up); failures are reported by the server's `error`
   *  message instead, so this map is simply dropped for them. */
  const pendingConnects = new Map<number, string>();

  /** Record a headless SFTP shell's server config under its sid (used by both
   *  the `sftpShell` announce and the `sftpOpenResult` bookkeeping). */
  function registerHeadlessShell(sid: number, server: WsServerConfig) {
    state.update((s) => ({
      ...s,
      headlessShells: { ...s.headlessShells, [sid]: server },
    }));
  }

  return {
    state,

    /** Send a client message (adds nothing else). */
    send: env.send,

    /** Remember up-front names/server configs for shells about to be created
     *  (consumed by the next `Shells` broadcast), then send `create`. */
    beginCreate(
      name: string,
      server: WsServerConfig | null,
      cwd: string | null = null,
    ) {
      state.update((s) => ({
        ...s,
        pendingNames: [...s.pendingNames, name],
        pendingServers: [...s.pendingServers, server],
      }));
      env.send({ create: [0, 0, server, cwd, name] });
    },

    /** Set the in-flight SFTP open request (component-driven). */
    setPendingOpen(flow: SftpOpenFlow | null) {
      pendingSftpOpen = flow;
    },

    /** Mark a pending "本机 SFTP" (open once the local shell is announced). */
    setPendingLocalSftp(value: boolean) {
      pendingLocalSftp = value;
    },

    /** Set the active (visible) terminal tab (user click / auto-switch). */
    setActive(sid: number) {
      state.update((s) => ({ ...s, activeId: sid }));
    },

    /** Route one server message. All mutations are state-store updates. */
    dispatch(message: WsServer) {
      if (message.hello) {
        env.onHello(message.hello);
        env.toast("success", tr("session.connectedToast"));
        // A new attach: its replay will follow; mark it unfinished.
        replayDone = false;
        return;
      }
      if (message.chunks) {
        const [shellId, chunks] = message.chunks;
        // First terminal output marks a server connection as established:
        // report success exactly once, then stream the output.
        const name = pendingConnects.get(shellId);
        if (name !== undefined) {
          pendingConnects.delete(shellId);
          env.toast("success", tr("session.connectedServer", { name }));
        }
        env.onChunks(shellId, chunks);
        return;
      }
      if (message.shells) {
        this.applyShells(message.shells);
        return;
      }
      if (message.shellsConfig) {
        // Reconnect replay: restore each visible remote shell's server config
        // (credentials were redacted server-side; re-merge the saved ones).
        const configs = message.shellsConfig;
        state.update((s) => {
          const shellServers = { ...s.shellServers };
          for (const [sid, cfg] of configs) shellServers[sid] = mergeReplayConfig(cfg);
          return { ...s, shellServers };
        });
        return;
      }
      if (message.headlessShells) {
        // Reconnect replay: restore the headless SFTP targets.
        for (const [sid, cfg] of message.headlessShells) {
          registerHeadlessShell(sid, mergeReplayConfig(cfg));
        }
        return;
      }
      if (message.activeShell !== undefined) {
        const sid = message.activeShell;
        state.update((s) => ({ ...s, activeId: sid }));
        // The replay's last state message: shell ids and the active tab are
        // now stable, so the component can restore editor tabs / file view.
        if (!replayDone) {
          replayDone = true;
          env.onReplayComplete();
        }
        return;
      }
      if (message.sftpShell) {
        // Headless SFTP shells announce their sid here; the browse happens on
        // the richer `sftpOpenResult` message.
        const sid = message.sftpShell;
        if (pendingSftpOpen?.server) {
          registerHeadlessShell(sid, pendingSftpOpen.server);
        }
        return;
      }
      if (message.sftpOpenResult) {
        this.applySftpOpenResult(message.sftpOpenResult);
        return;
      }
      if (message.sftpData) {
        // `sftpData` is the editor-read reply only; nothing else listens for
        // it (the file manager handles list/ok/error below).
        const [sid, name, bytes] = message.sftpData;
        env.routeEditorData(sid, name, bytes);
        return;
      }
      if (message.shellsMeta) {
        // Reconnect replay: restore tab titles (empty labels keep the default).
        const meta = message.shellsMeta;
        state.update((s) => {
          const baseTitles = { ...s.baseTitles };
          for (const [sid, label] of meta) {
            if (label) baseTitles[sid] = label;
          }
          return { ...s, baseTitles };
        });
        return;
      }
      if (message.sftpOk) {
        // An `sftpOk` also acknowledges a whole-file editor save (`sftpWrite`);
        // reset that editor's diff baseline before the file manager processes
        // the ack (upload chunks / move-copy batch / list refresh).
        const [sid, path] = message.sftpOk;
        env.routeEditorSave(sid, path);
        env.onFileMessage(message);
        return;
      }
      if (message.sftpWriteOk) {
        // A chunked-upload ack carries the written offset; route it to the
        // file manager's upload bookkeeping (never a whole-file save).
        env.onFileMessage(message);
        return;
      }
      if (message.pwd) {
        const [sid, path] = message.pwd;
        env.onPwd(sid, path);
        return;
      }
      if (message.sftpList || message.error) {
        env.onFileMessage(message);
      }
    },

    /** Consume pending create names/servers for newly added shells. */
    applyShells(shells: [number, WsWinsize][]) {
      const newIds = shells.map(([shellId]) => shellId);
      const added = newIds.filter((shellId) => !prevIds.includes(shellId));
      state.update((s) => {
        const baseTitles = { ...s.baseTitles };
        const shellServers = { ...s.shellServers };
        const pendingNames = [...s.pendingNames];
        const pendingServers = [...s.pendingServers];
        for (const shellId of added) {
          baseTitles[shellId] =
            pendingNames.shift() ?? tr("session.tabDefault");
          const server = pendingServers.shift() ?? null;
          shellServers[shellId] = server;
          if (server) pendingConnects.set(shellId, server.name);
        }
        // A pending server connection whose shell vanished before producing
        // any output failed — the server's `error` message reports it, so just
        // forget the success toast.
        for (const sid of [...pendingConnects.keys()]) {
          if (!newIds.includes(sid)) pendingConnects.delete(sid);
        }
        let activeId = s.activeId;
        if (added.length > 0) {
          // A new terminal was created; switch to it.
          activeId = added[added.length - 1];
        } else if (!newIds.includes(activeId)) {
          // The active terminal was closed; fall back to the last one.
          activeId = newIds[newIds.length - 1] ?? -1;
        }
        return {
          ...s,
          shells,
          activeId,
          baseTitles,
          shellServers,
          pendingNames,
          pendingServers,
        };
      });
      prevIds = newIds;
      env.onShellsListed();
      if (pendingLocalSftp) {
        // "本机 SFTP" clicked before any local shell existed; wait for the
        // created shell to be announced, then open its file system.
        pendingLocalSftp = false;
        const local = newIds.find((sid) => !get(state).shellServers[sid]);
        if (local !== undefined) {
          pendingSftpOpen = { fromSid: local, server: null };
          env.send({ sftpOpen: local });
        }
      }
    },

    /** Resolve the `sftpOpenResult`: bookkeep headless shells and rebind the
     *  file manager to the resolved target (keyed by its server). */
    applySftpOpenResult(result: [number, string, string, string | null]) {
      const [sid, cwd, , notice] = result;
      const flow = pendingSftpOpen;
      pendingSftpOpen = null;
      let serverName: string | null = null;
      if (flow?.server) {
        serverName = flow.server.name;
        registerHeadlessShell(sid, flow.server);
      }
      const key = serverTargetKey(flow?.server ?? null);
      // Only a server-list open targets a specific server regardless of the
      // active terminal; terminal / follow opens bind only if the active
      // terminal still matches (the FileManager guard checks this).
      env.openFileBrowser(sid, cwd, key, !!flow?.fromList);
      if (notice) {
        env.toast("info", notice);
      } else if (serverName && flow?.fromList) {
        // Only the server-list SFTP button reports an opening toast; the
        // terminal's file-manager toggle is silent (it follows the active
        // terminal's server).
        env.toast(
          "success",
          tr("session.connectedFiles", { name: serverName }),
        );
      }
    },
  };
}
