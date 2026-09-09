<script lang="ts">
  import type { Snippet } from "svelte";

  import { XIcon } from "$lib/icons";
  import { draggable, droppable } from "./dnd";

  interface Props {
    active?: boolean;
    title?: string;
    closeTitle?: string;
    onActivate: () => void;
    onClose: () => void;
    /** "terminal" tabs are larger with rounded-t styling and a close icon of
     *  14px; "editor" tabs are compact (10px text, 11px icon). */
    variant?: "terminal" | "editor";
    dragKey?: string | null;
    onTabDragStart?: (key: string) => void;
    onTabDragEnd?: () => void;
    onTabDragOver?: (key: string) => boolean;
    onTabDrop?: (key: string) => void;
    onTabDragLeave?: () => void;
    dragOver?: boolean;
    children?: Snippet;
  }

  let {
    active = false,
    title = "",
    closeTitle = "",
    onActivate,
    onClose,
    variant = "terminal",
    dragKey = null,
    onTabDragStart = () => {},
    onTabDragEnd = () => {},
    onTabDragOver = () => false,
    onTabDrop = () => {},
    onTabDragLeave = () => {},
    dragOver = false,
    children,
  }: Props = $props();

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
  onclick={onActivate}
  onkeydown={(event) => {
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
  {@render children?.()}
  <button
    class="rounded p-0.5 text-zinc-500 hover:bg-zinc-700 hover:text-zinc-200"
    onclick={(event) => {
      event.stopPropagation();
      onClose();
    }}
    title={closeTitle}
  >
    <XIcon size={variant === "terminal" ? "14" : "11"} />
  </button>
</div>
