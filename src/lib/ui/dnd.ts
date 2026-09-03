/** @file HTML5 drag-and-drop actions shared by the file manager
 *  (move-into-folder) and the server list (drag-to-reorder): both set up the
 *  `dataTransfer` on drag start, allow the drop with a "move" effect, and
 *  clear their drag state on `dragend`. The caller keeps its own source /
 *  highlight state and supplies the per-site guards and drop actions. */

import { get, writable } from "svelte/store";

/** Drag-to-reorder state machine shared by the server list and the terminal
 *  tab strip: one drag source + one highlighted drop target. `source` / `over`
 *  are Svelte stores so a template can reactively drive the drop-target
 *  highlight (`$over === id`); the caller performs the actual reorder in its
 *  drop handler after `drop()`.
 *
 *  Not used by the file manager: its drag is a move-into-folder operation
 *  (multi-select source, folder / up-row targets), not a positional reorder. */
export function createReorderDnd<T extends string | number>() {
  const source = writable<T | null>(null);
  const over = writable<T | null>(null);
  return {
    source,
    over,
    /** Record a drag source. */
    start(id: T) {
      source.set(id);
    },
    /** The drag ended: clear the source and any drop-target highlight. */
    end() {
      source.set(null);
      over.set(null);
    },
    /** Whether a drop onto `id` is allowed (a different item is being dragged),
     *  setting the drop-target highlight for it. */
    overTarget(id: T): boolean {
      const from = get(source);
      if (from === null || from === id) {
        over.set(null);
        return false;
      }
      over.set(id);
      return true;
    },
    /** The pointer left a drop target: clear its highlight. */
    leave() {
      over.set(null);
    },
    /** Consume the drag source and clear state, returning the source id (or
     *  null when no drag is active). Call from the drop handler. */
    drop(): T | null {
      const from = get(source);
      source.set(null);
      over.set(null);
      return from;
    },
  };
}

/** Make an element a drag source. `onStart` records the source state before
 *  the browser begins the drag (returning `false` aborts the drag, e.g. when
 *  it began on a button inside the element); `onEnd` runs on `dragend` (after
 *  a successful drop or a cancel) to clear drag state. */
export function draggable(
  node: HTMLElement,
  opts: {
    key: string;
    onStart(key: string, event: DragEvent): boolean | void;
    onEnd(): void;
  },
) {
  const onDragStart = (event: DragEvent) => {
    const allow = opts.onStart(opts.key, event);
    if (allow === false) {
      event.preventDefault();
      return;
    }
    if (event.dataTransfer) {
      event.dataTransfer.effectAllowed = "move";
      event.dataTransfer.setData("text/plain", opts.key);
    }
  };
  // Stable wrapper (like `droppable`): `update()` is called on every Svelte
  // re-render with a fresh inline object, so listeners must not be bound to a
  // specific `opts` identity or the old one would keep firing / never be
  // removed.
  const onDragEnd = () => opts.onEnd();
  node.addEventListener("dragstart", onDragStart);
  node.addEventListener("dragend", onDragEnd);
  return {
    update(next: typeof opts) {
      opts = next;
    },
    destroy() {
      node.removeEventListener("dragstart", onDragStart);
      node.removeEventListener("dragend", onDragEnd);
    },
  };
}

/** Make an element a drop target. `onDragOver` decides whether the drop is
 *  allowed (returning `true` also shows the "move" cursor) and sets its own
 *  hover highlight; `onDrop` performs the action; `onDragLeave` clears the
 *  highlight. */
export function droppable(
  node: HTMLElement,
  opts: {
    onDragOver(event: DragEvent): boolean;
    onDrop(event: DragEvent): void;
    onDragLeave(): void;
  },
) {
  // `dragenter` must be prevented too: the browser only paints the drop
  // cursor for a target whose `dragenter` was handled — otherwise a fast drag
  // over a small target (like the file manager's ".." up row) shows the
  // no-drop cursor even though the subsequent `drop` still works.
  const accept = (event: DragEvent) => {
    if (!opts.onDragOver(event)) return false;
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
    return true;
  };
  const onDragEnter = (event: DragEvent) => {
    accept(event);
  };
  const onDragOver = (event: DragEvent) => {
    accept(event);
  };
  const onDrop = (event: DragEvent) => {
    event.preventDefault();
    event.stopPropagation();
    opts.onDrop(event);
  };
  node.addEventListener("dragenter", onDragEnter);
  node.addEventListener("dragover", onDragOver);
  node.addEventListener("drop", onDrop);
  node.addEventListener("dragleave", opts.onDragLeave);
  return {
    update(next: typeof opts) {
      opts = next;
    },
    destroy() {
      node.removeEventListener("dragenter", onDragEnter);
      node.removeEventListener("dragover", onDragOver);
      node.removeEventListener("drop", onDrop);
      node.removeEventListener("dragleave", opts.onDragLeave);
    },
  };
}
