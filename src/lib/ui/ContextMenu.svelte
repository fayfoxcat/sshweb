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
  } from "svelte-feather-icons";

  import { lang, t } from "$lib/i18n";

  export let x: number;
  export let y: number;
  /** Size of the current selection (drives disabled states). */
  export let selectedCount: number;
  /** Whether a copy/cut is available to paste. */
  export let canPaste: boolean;
  /** True when the clipboard is a cut (paste becomes "移动"). */
  export let pasteMove: boolean;
  /** When pasting into a specific folder (right-clicked a folder), its name —
   *  shown in the paste tooltip; null pastes into the current directory. */
  export let pasteTargetName: string | null = null;
  export let onClose: () => void;
  /** Fired with a menu action id after the menu is closed. */
  export let onAction: (action: string) => void;

  let newSubmenuOpen = false;
  let uploadSubmenuOpen = false;

  function fire(action: string) {
    onClose();
    onAction(action);
  }

  function menuItemClass(disabled: boolean): string {
    return disabled
      ? "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-600"
      : "flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100";
  }
</script>

<div
  class="fixed z-[90] w-52 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl"
  style:left={`${Math.min(x, window.innerWidth - 216)}px`}
  style:top={`${Math.min(y, window.innerHeight - 330)}px`}
  on:contextmenu|preventDefault
>
  <!-- 新建 (flyout: 新建文件 / 新建文件夹) -->
  <div
    class="relative"
    on:mouseenter={() => (newSubmenuOpen = true)}
    on:mouseleave={() => (newSubmenuOpen = false)}
  >
    <button
      class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100 {newSubmenuOpen
        ? 'bg-zinc-700 text-zinc-100'
        : ''}"
      on:click|stopPropagation={() => fire("newFile")}
      title={t($lang, "file.titleNewMenu")}
    >
      <PlusIcon size="14" class="shrink-0" />
      <span class="flex-1">{t($lang, "file.menuNew")}</span>
      <ChevronRightIcon size="14" class="shrink-0 text-zinc-500" />
    </button>
    {#if newSubmenuOpen}
      <div
        class="absolute left-full top-0 z-[95] w-44 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl"
        on:mouseleave={() => (newSubmenuOpen = false)}
      >
        <button
          class={menuItemClass(false)}
          on:click|stopPropagation={() => fire("newFile")}
          title={t($lang, "file.titleNewFile")}
        >
          <FilePlusIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuNewFile")}</span>
        </button>
        <button
          class={menuItemClass(false)}
          on:click|stopPropagation={() => fire("newDir")}
          title={t($lang, "file.titleNewFolder")}
        >
          <FolderPlusIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuNewFolder")}</span>
        </button>
      </div>
    {/if}
  </div>
  <div class="my-1 border-t border-zinc-800" />
  <!-- 上传 (flyout: 上传文件 / 上传文件夹) -->
  <div
    class="relative"
    on:mouseenter={() => (uploadSubmenuOpen = true)}
    on:mouseleave={() => (uploadSubmenuOpen = false)}
  >
    <button
      class="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm text-zinc-300 transition-colors hover:bg-zinc-700 hover:text-zinc-100 {uploadSubmenuOpen
        ? 'bg-zinc-700 text-zinc-100'
        : ''}"
      on:click|stopPropagation={() => fire("uploadFiles")}
      title={t($lang, "file.titleUploadMenu")}
    >
      <UploadIcon size="14" class="shrink-0" />
      <span class="flex-1">{t($lang, "file.menuUpload")}</span>
      <ChevronRightIcon size="14" class="shrink-0 text-zinc-500" />
    </button>
    {#if uploadSubmenuOpen}
      <div
        class="absolute left-full top-0 z-[95] w-48 rounded-md border border-zinc-700 bg-zinc-900 py-1 shadow-xl"
        on:mouseleave={() => (uploadSubmenuOpen = false)}
      >
        <button
          class={menuItemClass(false)}
          on:click|stopPropagation={() => fire("uploadFiles")}
          title={t($lang, "file.titleUploadFiles")}
        >
          <UploadIcon size="14" class="shrink-0" />
          <span>{t($lang, "file.menuUploadFile")}</span>
        </button>
        <button
          class={menuItemClass(false)}
          on:click|stopPropagation={() => fire("uploadFolder")}
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
    on:click|stopPropagation={() => fire("rename")}
    title={t($lang, "file.titleRename")}
  >
    <TypeIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuRename")}</span>
  </button>
  <div class="my-1 border-t border-zinc-800" />
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    on:click|stopPropagation={() => fire("copy")}
    title={t($lang, "file.titleCopy")}
  >
    <CopyIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCopy")}</span>
  </button>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    on:click|stopPropagation={() => fire("cut")}
    title={t($lang, "file.titleCut")}
  >
    <ScissorsIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCut")}</span>
  </button>
  <button
    class={menuItemClass(!canPaste)}
    disabled={!canPaste}
    on:click|stopPropagation={() => fire("paste")}
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
  <div class="my-1 border-t border-zinc-800" />
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    on:click|stopPropagation={() => fire("download")}
    title={t($lang, "file.titleDownload")}
  >
    <DownloadIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuDownload")}</span>
  </button>
  <button
    class={menuItemClass(selectedCount === 0)}
    disabled={selectedCount === 0}
    on:click|stopPropagation={() => fire("delete")}
    title={t($lang, "file.titleDelete")}
  >
    <TrashIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuDelete")}</span>
  </button>
  <div class="my-1 border-t border-zinc-800" />
  <button
    class={menuItemClass(false)}
    on:click|stopPropagation={() => fire("copyPath")}
    title={t($lang, "file.titleCopyPath")}
  >
    <LinkIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuCopyPath")}</span>
  </button>
  <button
    class={menuItemClass(false)}
    on:click|stopPropagation={() => fire("sshInDir")}
    title={t($lang, "file.titleSshInDir")}
  >
    <TerminalIcon size="14" class="shrink-0" />
    <span>{t($lang, "file.menuSshInDir")}</span>
  </button>
</div>
