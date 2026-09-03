import { get, writable, type Writable } from "svelte/store";

/** Sidebar width bounds (pixels) and default viewport percentage. */
export const SIDEBAR_MIN_PX = 200;
export const SIDEBAR_MAX_PX = 560;
export const SIDEBAR_DEFAULT_PCT = 20; // ≈ 288px on a 1440px-wide window

/** Width of the sidebar (file manager + servers panel), shared across both:
 *  the panels are mutually exclusive, so editing one side keeps the other's
 *  remembered width. Session-local (not persisted, per design). */
export const sidebarWidth = writable<number>(SIDEBAR_DEFAULT_PCT);

/** CSS width for a sidebar, hidden when `open` is false. */
export function sidebarWidthCss(pct: number, open: boolean): string {
  if (!open) return "0px";
  return `clamp(${SIDEBAR_MIN_PX}px, ${pct}vw, ${SIDEBAR_MAX_PX}px)`;
}

/** Pointer-drag state machine shared by the sidebar resize handle and the
 *  editor window drag/resize. Attach the returned `onStart` to the drag
 *  handle and `onMove`/`onEnd` to the same element; pointer capture keeps
 *  the drag alive outside the element.
 *
 *  `start` snapshots the drag-start state and returns `false` to abort (the
 *  editor's title bar ignores drags that begin on a button/input). `move` and
 *  `end` are only invoked while a drag is active. */
export function createPointerDrag(opts: {
  start(event: PointerEvent): boolean;
  move(event: PointerEvent): void;
  end(): void;
}) {
  let active = false;
  function onStart(event: PointerEvent) {
    if (!opts.start(event)) return;
    (event.currentTarget as HTMLElement | null)?.setPointerCapture(
      event.pointerId,
    );
    active = true;
  }
  function onMove(event: PointerEvent) {
    if (active) opts.move(event);
  }
  function onEnd() {
    if (!active) return;
    active = false;
    opts.end();
  }
  return { onStart, onMove, onEnd };
}

/** Pointer-based sidebar resizing bound to a shared width store: drag deltas
 *  are converted into a viewport percentage (clamped to 200–560 px) and
 *  written to `width`. */
export function createSidebarResize(width: Writable<number>) {
  const dragging = writable(false);
  let startX = 0;
  let startPx = 0;

  const drag = createPointerDrag({
    start(event: PointerEvent) {
      dragging.set(true);
      startX = event.clientX;
      startPx = (get(width) / 100) * window.innerWidth;
      event.preventDefault();
      return true;
    },
    move(event: PointerEvent) {
      const px = Math.min(
        SIDEBAR_MAX_PX,
        Math.max(SIDEBAR_MIN_PX, startPx + (event.clientX - startX)),
      );
      width.set(Math.round((px / window.innerWidth) * 10000) / 100);
    },
    end() {
      dragging.set(false);
    },
  });

  return { dragging, ...drag };
}

/** Shared resize controller for both sidebars: the panels are mutually
 *  exclusive and share one width store, so the controller (and its `dragging`
 *  store) is created once at module level instead of per panel. */
export const sidebarResize = createSidebarResize(sidebarWidth);
export const sidebarDragging = sidebarResize.dragging;
