/**
 * @file 「命令片段」 sidebar data: the commands the current page saw submitted,
 * and the merge that turns them plus the shell's history file into the row of
 * history entries the panel shows.
 *
 * Why a merge is needed at all: the server reads `~/.bash_history` / zsh's
 * history file (see `crates/sshweb-server/src/session/history.rs`), but bash
 * only writes a command when the shell exits (or on `history -a`). Everything
 * typed in this page is therefore *missing* from that file, and a history mode
 * that showed only the file would look empty right after the user typed
 * something. The submitted commands are recorded here (session-local, newest
 * first) and merged in front of the file's entries.
 *
 * Session-local on purpose: after a refresh the file's entries are re-read, and
 * the commands typed before the refresh are (eventually) in that file — keeping
 * our own copy would only duplicate them under a second identity.
 */

import { writable } from "svelte/store";

/** Commands submitted in this page, per shell, newest first.
 *
 *  Exported as a store (not just the getters below) so the sidebar can derive
 *  its list from `$sessionCommands` and re-render the moment a command is
 *  submitted — `get()` inside a `$derived` would not subscribe. */
export const sessionCommands = writable<Record<number, string[]>>({});

/** How many of this page's own commands are kept per shell. */
const MAX_SESSION_COMMANDS = 200;

/** How many history rows the panel builds at most. */
export const MAX_HISTORY_ROWS = 800;

/** Record a command that was just submitted in `shellId`'s terminal.
 *
 *  Consecutive duplicates collapse (running `ls` five times is one row), and a
 *  command that moves to the front keeps only its newest position — the same
 *  rule the server applies to the history file, so the merged list never shows
 *  a command twice. */
export function recordSessionCommand(shellId: number, command: string): void {
  const text = command.trim();
  if (!text || shellId < 0) return;
  sessionCommands.update((all) => {
    const existing = all[shellId] ?? [];
    if (existing[0] === text) return all; // same command again: already first
    const next = [text, ...existing.filter((entry) => entry !== text)];
    return { ...all, [shellId]: next.slice(0, MAX_SESSION_COMMANDS) };
  });
}

/** Forget one shell's session commands (its terminal was closed). */
export function clearSessionCommands(shellId: number): void {
  sessionCommands.update((all) => {
    if (!(shellId in all)) return all;
    const next = { ...all };
    delete next[shellId];
    return next;
  });
}

/** Merge the session's own commands in front of the history file's entries.
 *
 *  Both inputs are newest-first; the result is newest-first, deduplicated
 *  (keeping the first occurrence, i.e. the most recent), and capped. */
export function mergeHistory(
  sessionEntries: string[],
  fileEntries: string[],
): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const entry of [...sessionEntries, ...fileEntries]) {
    const text = entry.trim();
    if (!text || seen.has(text)) continue;
    seen.add(text);
    out.push(text);
    if (out.length >= MAX_HISTORY_ROWS) break;
  }
  return out;
}

/** Case-insensitive substring match used by both sidebar modes' search box.
 *
 *  Deliberately a plain substring test rather than a fuzzy scorer: the list is
 *  short, the user is looking for a command they recognise, and a stable
 *  filter-then-scan order is easier to trust than a relevance ranking that can
 *  hide the entry that was just typed. */
export function matchesQuery(text: string, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return text.toLowerCase().includes(needle);
}
