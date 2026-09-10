<script lang="ts">
  import type { WsSftpEntry } from "$lib/protocol";
  import { formatDateTime, formatMode, formatSize } from "$lib/format";
  import { lang, t } from "$lib/i18n";

  interface Props {
    entry: WsSftpEntry;
    x: number;
    y: number;
  }

  let { entry, x, y }: Props = $props();
</script>

<div
  class="pointer-events-none fixed z-[80] rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-xs text-zinc-200 shadow-lg"
  style:left={`${x}px`}
  style:top={`${y}px`}
>
  <p class="mb-1 font-medium">{entry.name}</p>
  <p class="text-zinc-400">
    {t($lang, "file.hoverMode")}：{formatMode(entry.mode)}
  </p>
  <p class="text-zinc-400">
    {t($lang, "file.hoverModified")}：{formatDateTime(entry.modified)}
  </p>
  <p class="text-zinc-400">
    {t($lang, "file.hoverCreated")}：{formatDateTime(entry.created)}
  </p>
  {#if !entry.isDir}
    <p class="text-zinc-400">
      {t($lang, "file.hoverSize")}：{formatSize(entry.size)}
    </p>
  {/if}
</div>
