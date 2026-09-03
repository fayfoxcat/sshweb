type Sid = number; // u32

/** Size of a terminal window, see the Rust version. */
export type WsWinsize = {
  rows: number;
  cols: number;
};

/** A single entry in an SFTP directory listing, see the Rust version. */
export type WsSftpEntry = {
  name: string;
  isDir: boolean;
  /** True for symlinks (rendered as their target kind, but highlighted). */
  isLink: boolean;
  size: number;
  modified?: number;
  created?: number;
  /** Unix permission bits of the displayed target (incl. type bits). */
  mode: number;
};

/** Server message type, see the Rust version. */
export type WsServer = {
  hello?: string;
  shells?: [Sid, WsWinsize][];
  chunks?: [Sid, Uint8Array[]];
  sftpList?: [Sid, string, WsSftpEntry[], boolean];
  sftpOk?: [Sid, string];
  /** A chunked-upload write (`sftpWriteAt`) succeeded: echoes the written byte
   *  offset so the client can deduplicate retried chunks and resume exactly
   *  where the server acknowledged. */
  sftpWriteOk?: [Sid, string, number];
  sftpData?: [Sid, string, Uint8Array];
  sftpShell?: Sid;
  /** Result of an `sftpOpen` / `sftpConnect` probe: [sid to browse, initial
   *  directory, effective user, optional notice explaining a fallback]. */
  sftpOpenResult?: [Sid, string, string, string | null];
  /** Per-shell tab labels (sid → title), replayed on reconnect so titles
   *  survive a browser refresh. */
  shellsMeta?: [Sid, string][];
  /** Per-shell remote server configs (visible shells), replayed on reconnect
   *  so stats / ssh-in-dir / file-follow identity survive a refresh. */
  shellsConfig?: [Sid, WsServerConfig][];
  /** Headless SFTP shells and their server configs, replayed on reconnect. */
  headlessShells?: [Sid, WsServerConfig][];
  /** The active terminal tab, replayed on reconnect. */
  activeShell?: Sid;
  /** Reply to a `pwdRequest`: the shell's current working directory (empty
   *  when it could not be determined). Used to upload dropped files to the
   *  terminal's current directory. */
  pwd?: [Sid, string];
  error?: string;
};

/** A single SSH jump host in a chain (ProxyJump), see the Rust version. */
export type WsJumpHost = {
  host: string;
  port: number;
  username: string;
  password: string;
  /** ID of a saved server-side SSH key (public-key auth); empty/absent means
   *  password authentication with `password`. */
  keyId?: string | null;
};

/** Proxy configuration for reaching the SSH target, see the Rust version. */
export type WsProxyConfig = {
  kind: "http" | "socks5";
  host: string;
  port: number;
  username: string;
  password: string;
};

/** SOCKS5 隧道偏好（入站代理到远程内网；运行时由服务端监听）。 */
export type WsSocks5Tunnel = {
  /** 本地监听端口偏好（0 = 自动分配）。 */
  port: number;
  /** SOCKS5 认证用户名（可选；空 = 无认证）。 */
  username?: string;
  /** SOCKS5 认证密码（可选；username 非空时生效）。 */
  password?: string;
};

/** Remote SSH server connection parameters, see the Rust version. */
export type WsServerConfig = {
  name: string;
  host: string;
  port: number;
  username: string;
  password: string;
  encoding?: string;
  hosts?: WsJumpHost[];
  proxy?: WsProxyConfig | null;
  macs?: string[];
  /** Commands typed into the shell once the terminal starts (one per line). */
  startup?: string;
  /** Authentication method: "password" (default) or "key". */
  authMethod?: string;
  /** ID of a saved server-side SSH key used for public-key authentication. */
  keyId?: string;
  /** SOCKS5 隧道偏好（本机开放端口，访问远程内网服务）。 */
  socks5Tunnel?: WsSocks5Tunnel;
};

/** Client message type, see the Rust version. */
export type WsClient = {
  /** `create(x, y, serverConfig, cwd, label)` — `label` is the tab's base
   *  display title, persisted server-side so it survives a refresh. */
  create?: [
    number,
    number,
    WsServerConfig | null,
    string | null,
    string | null,
  ];
  sftpConnect?: WsServerConfig;
  /** Open SFTP for a terminal shell, following its current directory/user. */
  sftpOpen?: Sid;
  /** Set the active (visible) terminal tab. */
  setActive?: Sid;
  /** Reorder the terminal tabs (a permutation of the current sids); the
   *  server applies it and rebroadcasts `Shells` in the new order. */
  reorderShells?: Sid[];
  close?: Sid;
  resize?: [Sid, WsWinsize];
  data?: [Sid, Uint8Array];
  sftpList?: [Sid, string];
  sftpRead?: [Sid, string];
  sftpWrite?: [Sid, string, Uint8Array];
  sftpWriteAt?: [Sid, string, number, Uint8Array];
  sftpMkdir?: [Sid, string];
  sftpRemove?: [Sid, string, boolean];
  sftpRename?: [Sid, string, string];
  sftpCopy?: [Sid, string, string];
  /** Ask a shell for its current working directory (for uploading a dragged
   *  file to the terminal's `pwd`). The server replies with `pwd`. */
  pwdRequest?: Sid;
};
