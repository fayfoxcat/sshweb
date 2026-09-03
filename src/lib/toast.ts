/** @file Provides a simple, native toast library. */

import { writable } from "svelte/store";

export const toastStore = writable<(Toast & { expires: number })[]>([]);

export type Toast = {
  kind: "info" | "success" | "error";
  message: string;
  action?: string;
  onAction?: () => void;
};

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
      return [...$toasts];
    }
    return [...$toasts, obj];
  });
}
