/** @file Keyboard-shortcut Svelte actions shared across inputs/dialogs. */

/** Enter/Escape keyboard shortcut on an element. `onEnter` / `onEscape` fire
 *  on the matching key. `preventDefault` stops the browser default (e.g.
 *  submitting a form on Enter) and `stopPropagation` keeps parent key handlers
 *  from also reacting (the file manager's list row navigation). */
export function enterEscape(
  node: HTMLElement,
  opts: {
    onEnter?(): void;
    onEscape?(): void;
    /** Prevent the browser default action for the handled key. */
    preventDefault?: boolean;
    /** Stop the event from bubbling to parent key handlers. */
    stopPropagation?: boolean;
  },
) {
  function onKeydown(event: KeyboardEvent) {
    const handler =
      event.key === "Enter"
        ? opts.onEnter
        : event.key === "Escape"
        ? opts.onEscape
        : undefined;
    if (!handler) return;
    if (opts.preventDefault) event.preventDefault();
    if (opts.stopPropagation) event.stopPropagation();
    handler();
  }
  node.addEventListener("keydown", onKeydown);
  return {
    update(next: typeof opts) {
      opts = next;
    },
    destroy() {
      node.removeEventListener("keydown", onKeydown);
    },
  };
}
