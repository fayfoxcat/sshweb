<script lang="ts">
  import {
    AlertTriangleIcon,
    CheckIcon,
    InboxIcon,
    UploadCloudIcon,
    XIcon,
  } from "svelte-feather-icons";

  import { lang, t } from "$lib/i18n";
  import { cancelUploadTask, clearUploadTasks, uploadTasks } from "$lib/upload";

  let panelOpen = false;

  $: runningCount = $uploadTasks.filter((t) => t.status === "running").length;

  /** Upload progress clamped to [0, 100] (no-op for indeterminate tasks). */
  function progressPct(task: { done: number; total: number }): number {
    return task.total > 0
      ? Math.min(100, Math.round((task.done / task.total) * 100))
      : 0;
  }
</script>

{#if $uploadTasks.length > 0}
  <div class="fixed bottom-4 right-4 z-[85] flex flex-col items-end gap-2">
    {#if panelOpen}
      <div
        class="w-96 rounded-lg border border-zinc-700 bg-zinc-900/95 shadow-xl backdrop-blur"
      >
        <div
          class="flex items-center justify-between border-b border-zinc-800 px-3 py-2"
        >
          <span class="text-sm font-medium text-zinc-200"
            >{t($lang, "file.tasksTitle")}</span
          >
          <div class="flex items-center gap-1">
            {#if $uploadTasks.some((task) => task.status !== "running")}
              <button
                class="rounded px-2 py-1 text-[11px] text-zinc-400 transition-colors hover:bg-zinc-700 hover:text-zinc-200"
                on:click={clearUploadTasks}
                title={t($lang, "file.tasksClear")}
                >{t($lang, "file.tasksClear")}</button
              >
            {/if}
            <button
              class="rounded p-1 text-zinc-400 transition-colors hover:bg-zinc-700 hover:text-zinc-200"
              on:click={() => (panelOpen = false)}
              title={t($lang, "file.tasksCollapse")}
            >
              <XIcon size="14" />
            </button>
          </div>
        </div>
        <div class="max-h-72 overflow-y-auto p-2">
          {#each $uploadTasks as task (task.id)}
            <div class="mb-1 rounded-md px-2 py-1.5 hover:bg-zinc-800/60">
              <div class="flex items-center gap-2">
                <UploadCloudIcon size="14" class="shrink-0 text-sky-400" />
                <span
                  class="min-w-0 flex-1 truncate text-xs text-zinc-200"
                  title={task.name}>{task.name}</span
                >
                {#if task.status === "running"}
                  {#if task.total > 0}
                    <span class="shrink-0 text-[10px] text-zinc-400"
                      >{progressPct(task)}%</span
                    >
                  {:else}
                    <span class="shrink-0 text-[10px] text-zinc-400">…</span>
                  {/if}
                {:else if task.status === "error"}
                  <AlertTriangleIcon size="13" class="shrink-0 text-red-400" />
                {:else}
                  <CheckIcon size="13" class="shrink-0 text-emerald-400" />
                {/if}
                <button
                  class="shrink-0 rounded p-0.5 text-zinc-500 hover:bg-zinc-700 hover:text-zinc-200"
                  on:click={() => cancelUploadTask(task.id)}
                  title={t($lang, "file.tasksRemove")}
                >
                  <XIcon size="11" />
                </button>
              </div>
              {#if task.status === "running"}
                <div
                  class="mt-1 h-1 w-full overflow-hidden rounded-full bg-zinc-800"
                >
                  {#if task.total > 0}
                    <div
                      class="h-full rounded-full bg-sky-500 transition-[width] duration-200"
                      style:width={`${progressPct(task)}%`}
                    />
                  {:else}
                    <div
                      class="h-full w-1/3 animate-pulse rounded-full bg-zinc-500"
                    />
                  {/if}
                </div>
              {/if}
              {#if task.error}
                <p class="mt-0.5 text-[10px] text-red-400">{task.error}</p>
              {/if}
            </div>
          {/each}
        </div>
      </div>
    {/if}
    <button
      class="relative flex h-11 w-11 items-center justify-center rounded-full border border-zinc-700 bg-zinc-900/95 shadow-lg transition-colors hover:bg-zinc-800"
      on:click={() => (panelOpen = !panelOpen)}
      title={t($lang, "file.tasksTitle")}
    >
      <InboxIcon size="18" class="text-zinc-300" />
      {#if runningCount > 0}
        <span
          class="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-indigo-600 px-1 text-[9px] font-bold text-white"
          >{runningCount}</span
        >
      {/if}
    </button>
  </div>
{/if}
