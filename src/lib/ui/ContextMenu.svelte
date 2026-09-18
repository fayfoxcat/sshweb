<script lang="ts">
  import {
    ChevronRightIcon,
    ClipboardIcon,
    CopyIcon,
    DownloadIcon,
    FilePlusIcon,
    FolderPlusIcon,
    LinkIcon,
    PlusIcon,
    ScissorsIcon,
    TerminalIcon,
    TrashIcon,
    TypeIcon,
    UploadCloudIcon,
    UploadIcon,
  } from "$lib/icons";

  import { lang, t } from "$lib/i18n";

  interface Props {
    x: number;
    y: number;
    /** Size of the current selection (drives disabled states). */
    selectedCount: number;
    /** Whether a copy/cut is available to paste. */
    canPaste: boolean;
    /** True when the clipboard is a cut (paste becomes "移动"). */
    pasteMove: boolean;
    /** When pasting into a specific folder (right-clicked a folder), its name —
     *  shown in the paste tooltip; null pastes into the current directory. */
    pasteTargetName?: string | null;
    onClose: () => void;
    /** Fired with a menu action id after the menu is closed. */
    onAction: (action: string) => void;
  }

  let {
    x,
    y,
    selectedCount,
    canPaste,
    pasteMove,
    pasteTargetName = null,
    onClose,
    onAction,
  }: Props = $props();

  let newSubmenuOpen = $state(false);
  let uploadSubmenuOpen = $state(false);

  /** Width of the widest flyout (`w-48` = 192px + 2px border): what decides
   *  whether a flyout still fits to the right of its row. */
  const FLYOUT_WIDTH = 194;
  /** Gap kept between the menu and the viewport edges (px). */
  const EDGE_MARGIN = 4;

  let el = $state<HTMLDivElement>();
  /** Clamped position, or `null` until the menu has been measured: the menu's
   *  real size only exists once it has been rendered, so until then it stays at
   *  the pointer and is kept invisible — it must never paint a frame outside
   *  the viewport, which is the bug this replaced (see the effect below). */
  let placed = $state<{ left: number; top: number } | null>(null);
  /** Whether the flyouts have to open to the left of their row. */
  let flipFlyout = $state(false);

  $effect(() => {
    if (!el) return;
    const rect = el.getBoundingClientRect();
    // Clamp against the **measured** box. This is why the size is not
    // hardcoded: the menu's height depends on how many rows it has, and a
    // right-click near the bottom edge used to cut the last rows off whenever
    // the assumed height was too small.
    const width = rect.width;
    const height = rect.height;
    const nextLeft = Math.max(
      EDGE_MARGIN,
      Math.min(x, window.innerWidth - width - EDGE_MARGIN),
    );
    const nextTop = Math.max(
      EDGE_MARGIN,
      Math.min(y, window.innerHeight - height - EDGE_MARGIN),
    );
    // A flyout opens to the right of its row (`left-full`). Flip it, but only
    // when flipping is what actually keeps it on screen — otherwise a menu
    // sitting in the middle of a narrow window would keep its flyouts on the
    // left for no reason.
    const roomRight = window.innerWidth - (nextLeft + width + EDGE_MARGIN);
    const roomLeft = nextLeft - EDGE_MARGIN;
    flipFlyout = roomRight < FLYOUT_WIDTH && roomLeft > roomRight;
    // No vertical flip needs to exist: the clamped menu keeps every row inside
    // the viewport, and the two flyout rows sit in its first ~125px, so a
    // ~78px flyout cannot leave through the bottom.
    placed = { left: nextLeft, top: nextTop };
  });

  function fire(action: string) {
    onClose();
    onAction(action);
  }

  /** Click a menu button: stop propagation (keep the menu open handler from
   *  treating it as an outside click) then fire the action. */
  function stopClick(event: MouseEvent, action: string) {
    event.stopPropagation();
    fire(action);
  }

  function menuItemClass(disabled: boolean): string {
    return disabled
      ? "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-600"
      : "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100";
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="fixed z-[90] w-52 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl"
  class:invisible={!placed}
  bind:this={el}
  style:left={`${placed?.left ?? x}px`}
  style:top={`${placed?.top ?? y}px`}
  oncontextmenu={(event) => event.preventDefault()}
>
  <!-- 新建 (flyout: 新建文件 / 新建文件夹) -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="relative"
    onmouseenter={() => (newSubmenuOpen = true)}
    onmouseleave={() => (newSubmenuOpen = false)}
  >
    <button
      class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100 {newSubmenuOpen
        ? 'bg-zinc-700 text-zinc-100'
        : ''}"
      onclick={(event) => stopClick(event, "newFile")}
      title={t($lang, "file.titleNewMenu")}
    >
      <PlusIcon size="14" class="shrink-0" />
      <span class="flex-1">{t($lang, "file.menuNew")}</span>
      <ChevronRightIcon size="14" class="shrink-0 text-zinc-500" />
    </button>
    {#if newSubmenuOpen}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="absolute top-0 z-[95] w-44 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl {flipFlyout
          ? 'right-full'
          : 'left-full'}"
        onmouseleave={() => (newSubmenuOpen = false)}
      >
        <button
          class={menuItemClass(false)}
          onclick={(event) => stopClick(event, "newFile")}
          title={t($lang, "file.titleNewFile")}
        >
          <FilePlusIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuNewFile")}</span>
        </button>
        <button
          class={menuItemClass(false)}
          onclick={(event) => stopClick(event, "newDir")}
          title={t($lang, "file.titleNewFolder")}
        >
          <FolderPlusIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuNewFolder")}</span>
        </button>
      </div>
    {/if}
  </div>
  <div class="my-1 border-t border-zinc-800"></div>
  <!-- 上传 (flyout: 上传文件 / 上传文件夹) -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="relative"
    onmouseenter={() => (uploadSubmenuOpen = true)}
    onmouseleave={() => (uploadSubmenuOpen = false)}
  >
    <button
      class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100 {uploadSubmenuOpen
        ? 'bg-zinc-700 text-zinc-100'
        : ''}"
      onclick={(event) => stopClick(event, "uploadFiles")}
      title={t($lang, "file.titleUploadMenu")}
    >
      <UploadIcon size="14" class="shrink-0" />
      <span class="flex-1">{t($lang, "file.menuUpload")}</span>
      <ChevronRightIcon size="14" class="shrink-0 text-zinc-500" />
    </button>
    {#if uploadSubmenuOpen}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="absolute top-0 z-[95] w-48 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl {flipFlyout
          ? 'right-full'
          : 'left-full'}"
        onmouseleave={() => (uploadSubmenuOpen = false)}
      >
        <button
          class={menuItemClass(false)}
          onclick={(event) => stopClick(event, "uploadFiles")}
          title={t($lang, "file.titleUploadFiles")}
        >
          <UploadIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuUploadFile")}</span>
        </button>
        <button
          class={menuItemClass(false)}
          onclick={(event) => stopClick(event, "uploadFolder")}
          title={t($lang, "file.titleUploadFolder")}
        >
          <UploadCloudIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuUploadFolder")}</span>
        </button>
      </div>
    {/if}
  </div>
  <button
    class={menuItemClass(selectedCount !== 1)}
    disabled={selectedCount !== 1}
    onclick={(event) => stopClick(event, "rename")}
    title={t($lang, "file.titleRename")}
  >
    <TypeIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuRename")}</span>
  </button>
  <div class="my-1 border-t border-zinc-800"></div>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    onclick={(event) => stopClick(event, "copy")}
    title={t($lang, "file.titleCopy")}
  >
    <CopyIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCopy")}</span>
  </button>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    onclick={(event) => stopClick(event, "cut")}
    title={t($lang, "file.titleCut")}
  >
    <ScissorsIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCut")}</span>
  </button>
  <button
    class={menuItemClass(!canPaste)}
    disabled={!canPaste}
    onclick={(event) => stopClick(event, "paste")}
    title={pasteTargetName
      ? t($lang, "file.titlePasteInto", { dir: pasteTargetName })
      : t($lang, "file.titlePaste")}
  >
    <ClipboardIcon size="14" class="shrink-0" />
    <span
      >{pasteMove
        ? t($lang, "file.menuPasteMove")
        : t($lang, "file.menuPaste")}</span
    >
  </button>
  <div class="my-1 border-t border-zinc-800"></div>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    onclick={(event) => stopClick(event, "download")}
    title={t($lang, "file.titleDownload")}
  >
    <DownloadIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuDownload")}</span>
  </button>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    onclick={(event) => stopClick(event, "delete")}
    title={t($lang, "file.titleDelete")}
  >
    <TrashIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuDelete")}</span>
  </button>
  <div class="my-1 border-t border-zinc-800"></div>
  <button
    class={menuItemClass(false)}
    onclick={(event) => stopClick(event, "copyPath")}
    title={t($lang, "file.titleCopyPath")}
  >
    <LinkIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCopyPath")}</span>
  </button>
  <button
    class={menuItemClass(false)}
    onclick={(event) => stopClick(event, "sshInDir")}
    title={t($lang, "file.titleSshInDir")}
  >
    <TerminalIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuSshInDir")}</span>
  </button>
</div>
