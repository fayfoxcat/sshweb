import { derived, writable } from "svelte/store";

import { EDITORS_STATE_KEY } from "$lib/constants";
import { storageGet, storageSet } from "$lib/storage";

type EditorsState = {
  /** Open editor composite keys (`sid:path`), in **open order** (the tab bar
   *  never reorders). A path may exist on several servers; each key is a
   *  distinct editor, so cross-server files never collide. */
  open: string[];
  /** Composite key → path (for display and the Editor's `filePath`). */
  pathByKey: Record<string, string>;
  /** Composite key → shell (terminal sid) the editor was opened from — a file
   *  edit is scoped to that server, so switching the active terminal never
   *  re-targets an editor to a different server. */
  shellByKey: Record<string, number>;
  /** Keys currently minimized (hidden, kept as tab). */
  minimized: Set<string>;
  /** Keys with unsaved edits (for the tab indicator). */
  dirty: Set<string>;
  /** The focused editor key (brought to front); independent of tab order. */
  active: string | null;
};

function without<T>(set: Set<T>, p: T): Set<T> {
  const next = new Set(set);
  next.delete(p);
  return next;
}

/** Composite key binding a file to the shell it belongs to: `sid:path`. A path
 *  may exist on several servers, and each editor is scoped to the shell it was
 *  opened on, so reads, saves and drafts must never collide across servers. */
export function editorKey(sid: number, path: string): string {
  return `${sid}:${path}`;
}

/** Saved editor-tab state (open / minimized / dirty / active), read from
 *  sessionStorage so a refresh restores the tab bar (content is restored via
 *  drafts). Older saves keyed editors by plain path; they are migrated to
 *  composite `sid:path` keys (the path's shell, or a sentinel `-1`). */
export function loadEditorState(): EditorsState | null {
  return storageGet<EditorsState | null>(EDITORS_STATE_KEY, null, (raw) => {
    const parsed = JSON.parse(raw) as {
      open?: string[];
      shellByPath?: Record<string, number>;
      pathByKey?: Record<string, string>;
      shellByKey?: Record<string, number>;
      minimized?: string[];
      dirty?: string[];
      active?: string | null;
    };
    if (!Array.isArray(parsed.open)) return null;

    let open: string[] = parsed.open;
    let pathByKey = parsed.pathByKey ?? {};
    let shellByKey = parsed.shellByKey ?? {};
    const legacyShells = parsed.shellByPath ?? {};

    if (!parsed.pathByKey || !parsed.shellByKey) {
      // Legacy: `open` holds plain paths, `shellByPath` maps path → sid.
      const nextOpen: string[] = [];
      const nextPath: Record<string, string> = {};
      const nextShell: Record<string, number> = {};
      for (const p of parsed.open) {
        const sid = legacyShells[p] ?? -1;
        const key = editorKey(sid, p);
        nextOpen.push(key);
        nextPath[key] = p;
        nextShell[key] = sid;
      }
      open = nextOpen;
      pathByKey = nextPath;
      shellByKey = nextShell;
    }

    // Backward compat: older saves have no `active`; default to the topmost.
    const active =
      parsed.active && open.includes(parsed.active)
        ? parsed.active
        : open[open.length - 1] ?? null;
    return {
      open,
      pathByKey,
      shellByKey,
      minimized: new Set(parsed.minimized ?? []),
      dirty: new Set(parsed.dirty ?? []),
      active,
    };
  });
}

/** The topmost non-minimized editor key in `open` order, or null when all are
 *  minimized (used when the active editor is minimized/closed). */
function fallbackActive(open: string[], minimized: Set<string>): string | null {
  for (let i = open.length - 1; i >= 0; i--) {
    if (!minimized.has(open[i])) return open[i];
  }
  return open.length ? open[open.length - 1] : null;
}

/** Multi-file editor tab model (store-backed so Svelte templates stay
 *  reactive). The editor DOM refs remain in the owning component. Persists the
 *  tab bar to sessionStorage on every change so it survives a refresh.
 *
 *  Every editor is identified by a **composite key `sid:path`**, so the same
 *  path on two different servers opens as two separate tabs (they share the
 *  basename label but are distinct editors with their own content/drafts).
 *
 *  Tab order = open order and never changes; the focused editor is tracked
 *  separately via `active`.
 *
 *  The store is **seeded from the persisted state** so the first subscription
 *  emit writes the restored tabs back unchanged — seeding empty would clobber
 *  the saved tab bar before `restoreEditors` reads it. */
export function createEditors() {
  const saved = loadEditorState();
  const state = writable<EditorsState>(
    saved ?? {
      open: [],
      pathByKey: {},
      shellByKey: {},
      minimized: new Set(),
      dirty: new Set(),
      active: null,
    },
  );

  // Persist the tab bar (open/minimized/dirty/active/paths/shells) across
  // refreshes.
  state.subscribe((s) => {
    storageSet(EDITORS_STATE_KEY, {
      open: s.open,
      pathByKey: s.pathByKey,
      shellByKey: s.shellByKey,
      minimized: [...s.minimized],
      dirty: [...s.dirty],
      active: s.active,
    });
  });

  const active = derived(state, ($s) => $s.active);
  /** Merge a partial patch over the current state, carrying through the
   *  untouched fields (single boilerplate point for the tab-model updates). */
  function update(patch: (s: EditorsState) => Partial<EditorsState>) {
    state.update((s) => ({ ...s, ...patch(s) }));
  }

  return {
    subscribe: state.subscribe,
    active,
    /** Open a path on a shell (append the composite key if new) and focus it.
     *  Re-opening the same key never reorders the tab bar; the same path on a
     *  different shell opens a separate tab. */
    open(p: string, shellId: number) {
      const key = editorKey(shellId, p);
      update((s) => ({
        open: s.open.includes(key) ? s.open : [...s.open, key],
        pathByKey: { ...s.pathByKey, [key]: p },
        shellByKey: { ...s.shellByKey, [key]: shellId },
        minimized: without(s.minimized, key),
        active: key,
      }));
    },
    activate(key: string) {
      update((s) => ({
        minimized: without(s.minimized, key),
        active: key,
      }));
    },
    minimize(key: string) {
      update((s) => {
        const minimized = new Set([...s.minimized, key]);
        const active =
          s.active === key ? fallbackActive(s.open, minimized) : s.active;
        return { minimized, active };
      });
    },
    close(key: string) {
      update((s) => {
        const open = s.open.filter((x) => x !== key);
        const pathByKey = { ...s.pathByKey };
        delete pathByKey[key];
        const shellByKey = { ...s.shellByKey };
        delete shellByKey[key];
        const minimized = without(s.minimized, key);
        const dirty = without(s.dirty, key);
        const active =
          s.active === key ? fallbackActive(open, minimized) : s.active;
        return { open, pathByKey, shellByKey, minimized, dirty, active };
      });
    },
    markDirty(key: string, dirty: boolean) {
      update((s) => {
        const next = without(s.dirty, key);
        if (dirty) next.add(key);
        return { dirty: next };
      });
    },
  };
}
