/** @file Clipboard helpers with a legacy fallback, shared by the terminal
 *  (selection copy / right-click paste), the file manager (copy absolute
 *  path) and the server panel (copy public key). Previously each caller kept
 *  its own `navigator.clipboard` + hidden-textarea fallback; this module is
 *  the single place where read/write is attempted. */

/** Run `fn` with a hidden off-page textarea attached (clipboard fallback). */
function withTempTextarea<T>(fn: (ta: HTMLTextAreaElement) => T): T {
  const ta = document.createElement("textarea");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  try {
    return fn(ta);
  } finally {
    document.body.removeChild(ta);
  }
}

/** Copy text to the OS clipboard. Resolves `true` on success.
 *
 *  Tries the async Clipboard API first; when it is unavailable or rejects
 *  (e.g. permissions denied / insecure context), falls back to a hidden
 *  textarea + `document.execCommand("copy")`.
 */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Fall through to the legacy path.
    }
  }
  try {
    return withTempTextarea((ta) => {
      ta.value = text;
      ta.select();
      return document.execCommand("copy");
    });
  } catch {
    return false;
  }
}

/** Result of a clipboard read: success (with the text, which may be empty —
 *  an empty read is a legitimate "nothing to paste", not an error), or why the
 *  read failed. */
export type ClipboardRead =
  | { ok: true; text: string }
  | { ok: false; reason: "insecure" | "denied" };

/** Read text from the OS clipboard.
 *
 *  Browsers only expose clipboard **reads** to a page in a secure context
 *  (HTTPS, or `http://localhost` / `http://127.0.0.1`) via the async Clipboard
 *  API, and only while the document is focused and processing a user gesture.
 *  On plain HTTP from a LAN origin there is no API at all, and the legacy
 *  `document.execCommand("paste")` fallback is blocked by every modern
 *  browser — so the honest result there is `"insecure"`, not a silent empty.
 */
export async function readClipboard(): Promise<ClipboardRead> {
  if (!window.isSecureContext || !navigator.clipboard?.readText) {
    return { ok: false, reason: "insecure" };
  }
  try {
    return { ok: true, text: await navigator.clipboard.readText() };
  } catch {
    // Permission denied, or the user gesture / document focus was lost
    // between the right-click and the async read resolving.
    return { ok: false, reason: "denied" };
  }
}
