/** Cross-cutting numeric constants shared across the frontend.
 *
 *  Keeping these in one module prevents the same literal (e.g. `5000`) being
 *  reused with different meanings in unrelated places and drifting from its
 *  i18n copy.
 */

/** Max terminal tabs per session (mirrored by the `session.limit` i18n text,
 *  which interpolates this value via `{n}`). */
export const MAX_TERMINALS = 14;

/** Auth-state probe throttle after a WebSocket disconnect (ms). */
export const AUTH_PROBE_THROTTLE_MS = 5000;

/** Default terminal scrollback lines. */
export const DEFAULT_SCROLLBACK = 5000;

/** Max file-list rows rendered at once (larger directories are searchable). */
export const MAX_FILE_ROWS = 2000;

/** Cap for multi-item archive filenames (chars, keeps at least one name). */
export const ARCHIVE_NAME_CAP = 40;

/** Custom file-tooltip hover delay (ms). */
export const TOOLTIP_DELAY_MS = 500;

/** Server-stats polling interval (ms). */
export const STATS_POLL_MS = 1000;

/** WebSocket reconnect delay after a disconnect (ms). */
export const RECONNECT_DELAY_MS = 500;

/** Number of messages to queue while the WebSocket is disconnected. */
export const SROCKET_BUFFER_SIZE = 64;

/** Chunk size for chunked SFTP uploads (bytes, see `sftpWriteAt`). Leaves room
 *  for the SFTP WRITE request header inside the 256 KiB packet limit. */
export const UPLOAD_CHUNK = 240 * 1024;

/** Upload chunk-ack watchdog: if a chunk isn't acknowledged within this long
 *  (ms), it is retried — a dropped `sftpWriteOk` (the server's 512-bounded
 *  output queue is best-effort) must not stall an upload forever. Longer than
 *  the server's 30s SFTP-op timeout so a legitimate slow write is not retried
 *  prematurely. */
export const UPLOAD_ACK_TIMEOUT_MS = 60_000;

/** Consecutive watchdog timeouts before an upload is marked failed. */
export const UPLOAD_MAX_RETRIES = 3;

/** sessionStorage key for the upload task list: survives a browser refresh so
 *  the transfer-panel button and finished/interrupted records stay visible
 *  until the user clears them. */
export const UPLOAD_TASKS_KEY = "sshweb-upload-tasks";

/** Default SSH port when a server form omits it. */
export const DEFAULT_SSH_PORT = 22;

/** Default SOCKS5 proxy port when a proxy form omits it. */
export const DEFAULT_SOCKS_PORT = 1080;

/** localStorage keys (legacy migration + client-side preferences). */
export const STORAGE_KEY_SERVERS = "sshx-servers-store";
export const STORAGE_KEY_LANGUAGE = "sshweb-language";
export const STORAGE_KEY_SETTINGS = "sshweb-settings-store";

/** sessionStorage key for the stable session id: reconnecting to the same
 *  key after a browser refresh reattaches to the live server-side session
 *  (terminals/processes are preserved). */
export const SESSION_STORAGE_KEY = "sshweb-session-key";

/** sessionStorage keys for editor-tab / unsaved-draft persistence across a
 *  refresh (drafts prevent losing unsaved edits on reload). */
export const EDITORS_STATE_KEY = "sshweb-editors-state";
export const EDITOR_DRAFTS_KEY = "sshweb-editor-drafts";

/** Editor drafts are skipped above this size (bytes) to keep the
 *  sessionStorage budget sane. */
export const EDITOR_DRAFT_MAX_BYTES = 1 << 20;

/** Unsaved editor drafts are discarded after this long without any edit
 *  (ms). Stale drafts could otherwise be overlaid onto a file that changed a
 *  lot on disk since. */
export const EDITOR_DRAFT_TTL_MS = 7 * 24 * 60 * 60 * 1000; // 7 days

/** sessionStorage key for the file-manager view (browsed shell + path),
 *  restored after a refresh. */
export const SFTP_VIEW_STORAGE_KEY = "sshweb-sftp-view";

/** sessionStorage key for which side panel is open ("servers" | "files" |
 *  null), restored after a refresh. */
export const PANEL_STORAGE_KEY = "sshweb-panel";
