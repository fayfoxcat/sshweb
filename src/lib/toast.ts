/** @file Provides a simple, native toast library. */

import { writable } from "svelte/store";

/** A toast in the store: the caller's content plus the auto-dismiss deadline.
 *  `remainingMs` is set only while the pointer rests on the toast (see
 *  `pauseToast`) — the countdown is frozen by pushing `expires` far out, and
 *  the remaining time is kept here to restore it on the way out. */
export type StoredToast = Toast & { expires: number; remainingMs?: number };

export const toastStore = writable<StoredToast[]>([]);

export type Toast = {
  kind: "info" | "success" | "error";
  message: string;
  action?: string;
  onAction?: () => void;
};

/** Deadline used while a toast is paused. Any value beyond a few days works;
 *  the container's sweep only compares it against `Date.now()`. */
const PAUSED_EXPIRES = Number.MAX_SAFE_INTEGER;

/** Show an error toast from an unknown thrown value (Error message or the
 *  value itself) — the single place the repeated `(err as Error).message`
 *  catch blocks across the UI funnel through. */
export function toastError(err: unknown, duration?: number): void {
  const message = err instanceof Error ? err.message : String(err);
  makeToast({ kind: "error", message }, duration);
}

export function makeToast(toast: Toast, duration = 3000) {
  const obj = Object.assign({ expires: Date.now() + duration }, toast);
  toastStore.update(($toasts) => {
    // Deduplicate identical (kind, message) toasts: refresh the existing one
    // instead of stacking copies. A burst of identical errors (e.g. one per
    // failed upload chunk) must only show a single toast.
    const existing = $toasts.find(
      (t) => t.kind === obj.kind && t.message === obj.message,
    );
    if (existing) {
      existing.expires = obj.expires;
      existing.action = obj.action;
      existing.onAction = obj.onAction;
      // Drop a frozen countdown: the refresh restarts the full duration, so a
      // stale remaining time would cut the new one short on the way out.
      delete existing.remainingMs;
      return [...$toasts];
    }
    return [...$toasts, obj];
  });
}

/** Freeze a toast's countdown while the pointer rests on it, so a message that
 *  needs reading or copying does not vanish mid-gesture.
 *
 *  Mutates in place on purpose: the container identifies toasts by object
 *  identity (both for the sweep and for `{#each}` keys), so replacing the
 *  object would tear down and rebuild the element under the pointer. */
export function pauseToast(toast: StoredToast): void {
  if (toast.remainingMs !== undefined) return; // Already paused (nested hover).
  toast.remainingMs = Math.max(toast.expires - Date.now(), 0);
  toast.expires = PAUSED_EXPIRES;
}

/** Resume a paused toast's countdown from where it stopped. */
export function resumeToast(toast: StoredToast): void {
  if (toast.remainingMs === undefined) return;
  toast.expires = Date.now() + toast.remainingMs;
  delete toast.remainingMs;
}
