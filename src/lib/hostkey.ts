/** @file SSH host-key change handling (已知坑 80).
 *
 *  A target that was reinstalled (or replaced) presents a new host key, which
 *  the TOFU check (已知坑 67) correctly rejects — every connection to it then
 *  fails until the recorded fingerprint is dropped. That fingerprint is only
 *  ever written on a *first* connect, so there is no way back without an
 *  explicit action.
 *
 *  Forgetting it is a security-relevant decision (it is what protects against
 *  a man-in-the-middle), so it never happens silently: `hostKeyPrompt` is
 *  filled from a failure path, a single `HostKeyDialog` shows both fingerprints,
 *  and only the user's confirmation calls the server.
 *
 *  Every path that can hit the check fills it, because each one hides the
 *  failure differently:
 *    - `Servers.svelte` — the connection test, the only path that gets the
 *      structured `hostKeyChanged` body (`hostKeyPromptFrom`);
 *    - `FileManager.svelte` — the terminal's connect failure, which the server
 *      reports as plain text (`hostKeyPromptFromMessage`);
 *    - `session/runtime.ts` — the SFTP open notice, likewise text but on the
 *      `sftpOpenResult` channel instead of the `error` one.
 */

import { writable } from "svelte/store";

import { ApiError } from "./api";

/** A host-key mismatch awaiting the user's decision. */
export interface HostKeyPrompt {
  /** `user@host:port` — the key the server stores the fingerprint under. */
  target: string;
  /** Fingerprint recorded on the first connection, when the failing request
   *  sent structured detail (the connection test does). */
  expected?: string;
  /** Fingerprint the server presented this time (same caveat). */
  actual?: string;
  /** Human-readable failure text. The terminal / SFTP paths report the failure
   *  as a plain string rather than structured fields; it already names both
   *  fingerprints, so the dialog shows it verbatim when they are absent. */
  message: string;
}

/** The mismatch awaiting the user, or null. Rendered by the single
 *  `HostKeyDialog` mounted in the app shell. */
export const hostKeyPrompt = writable<HostKeyPrompt | null>(null);

/** The marker every host-key mismatch message carries
 *  (`ssh::HostKeyChanged::Display`). Terminal and SFTP failures arrive as a
 *  plain string, so recognising them there means matching this text — the same
 *  string coupling the upload error prefixes already rely on (坑 38), and
 *  "server-side messages keep their original Chinese text" is documented
 *  behaviour (`i18n/index.ts`). Keep in sync with
 *  `crates/sshweb-server/src/ssh.rs`. */
const MESSAGE_MARKER = "的 SSH 主机密钥已变更";

/** The mismatch behind a failed API request, if that is what it was. */
export function hostKeyPromptFrom(err: unknown): HostKeyPrompt | null {
  if (!(err instanceof ApiError)) return null;
  const raw = err.body.hostKeyChanged;
  if (!raw || typeof raw !== "object") return null;
  const { target, expected, actual } = raw as Record<string, unknown>;
  if (
    typeof target !== "string" ||
    typeof expected !== "string" ||
    typeof actual !== "string"
  ) {
    return null;
  }
  return { target, expected, actual, message: err.message };
}

/** The mismatch behind a plain failure message, if that is what it was.
 *
 *  The message always names the target right before the marker, but it may
 *  carry a prefix: the terminal reports `连接 <name> 失败：<target> 的 SSH 主机
 *  密钥已变更：…` and the SFTP notice is the bare `<target> 的 …`. So the
 *  target is the last `user@host:port` token before the marker rather than
 *  everything up to it — the identity the server checked, which is what
 *  `forgetHostKey` needs (a server *name* would not match any record). */
export function hostKeyPromptFromMessage(
  message: string,
): HostKeyPrompt | null {
  const at = message.indexOf(MESSAGE_MARKER);
  if (at <= 0) return null;
  const match = message.slice(0, at).match(/([^\s：@]+@[^\s：]+:\d+)\s*$/);
  if (!match) return null;
  return { target: match[1], message };
}
