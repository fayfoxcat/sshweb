<script lang="ts">
  import { XIcon } from "svelte-feather-icons";

  import { draggable, droppable } from "./dnd";

  export let active = false;
  export let title = "";
  export let closeTitle = "";
  export let onActivate: () => void;
  export let onClose: () => void;
  /** "terminal" tabs are larger with rounded-t styling and a close icon of
   *  14px; "editor" tabs are compact (10px text, 11px icon). */
  export let variant: "terminal" | "editor" = "terminal";

  // ---- Drag-to-sort (terminal tabs) --------------------------------------
  // When `dragKey` is set the whole tab (except the close button) is a drag
  // handle and the tab is a drop target. `onTabDragOver(key)` decides whether
  // a drop onto this tab is allowed; `onTabDrop(key)` performs the reorder.
  // `dragOver` (the reorder-target highlight) is a prop driven by the parent's
  // shared reorder-dnd state (`createReorderDnd`), so all tabs read one source.
  export let dragKey: string | null = null;
  export let onTabDragStart: (key: string) => void = () => {};
  export let onTabDragEnd: () => void = () => {};
  export let onTabDragOver: (key: string) => boolean = () => false;
  export let onTabDrop: (key: string) => void = () => {};
  export let onTabDragLeave: () => void = () => {};
  /** Highlight while another tab is dragged over this one (reorder target). */
  export let dragOver = false;

  /** Whether a drag started on the close button (which must not drag the tab). */
  function onCloseButton(event: DragEvent): boolean {
    const target = event.target as HTMLElement | null;
    return Boolean(target?.closest("button"));
  }
</script>

<div
  class="group flex shrink-0 cursor-pointer select-none items-center transition-colors {variant ===
  'terminal'
    ? 'max-w-[220px] gap-2 rounded-t-md border border-b-0 px-3 py-1.5 text-sm'
    : 'gap-1 rounded-md border px-2 py-0.5 text-[10px]'}"
  class:border-zinc-700={active && variant === "terminal"}
  class:bg-[#111111]={active && variant === "terminal"}
  class:border-zinc-600={active && variant === "editor"}
  class:bg-zinc-800={active && variant === "editor"}
  class:text-zinc-100={active}
  class:border-transparent={!active}
  class:text-zinc-400={!active}
  class:hover:bg-zinc-800={!active}
  class:hover:text-zinc-200={!active && variant === "terminal"}
  class:cursor-grab={dragKey !== null}
  class:bg-zinc-700={dragOver}
  class:border-indigo-500={dragOver}
  role="tab"
  aria-selected={active}
  draggable={dragKey !== null}
  tabindex={variant === "terminal" ? 0 : -1}
  on:click={onActivate}
  on:keydown={(event) => {
    if (
      variant === "terminal" &&
      (event.key === "Enter" || event.key === " ")
    ) {
      event.preventDefault();
      onActivate();
    }
  }}
  {title}
  use:draggable={{
    key: dragKey ?? "",
    onStart: (_key, event) => {
      if (dragKey === null) return false;
      // Don't start a tab drag from the close button.
      if (onCloseButton(event)) return false;
      onTabDragStart(dragKey);
      return true;
    },
    onEnd: () => {
      onTabDragEnd();
    },
  }}
  use:droppable={{
    onDragOver: () => dragKey !== null && onTabDragOver(dragKey),
    onDrop: () => {
      if (dragKey !== null) onTabDrop(dragKey);
    },
    onDragLeave: () => {
      onTabDragLeave();
    },
  }}
>
  <slot />
  <button
    class="rounded p-0.5 text-zinc-500 hover:bg-zinc-700 hover:text-zinc-200"
    on:click={(event) => {
      event.stopPropagation();
      onClose();
    }}
    title={closeTitle}
  >
    <XIcon size={variant === "terminal" ? "14" : "11"} />
  </button>
</div>
