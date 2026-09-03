<script lang="ts">
  import { onDestroy, onMount } from "svelte";

  import { ChevronsDownIcon, ChevronsUpIcon } from "svelte-feather-icons";

  /** Startup command snippet: a one-line input (matching the other server
   *  form fields) that can expand into a three-line shell-highlighted
   *  CodeMirror editor. */
  import { lang, t } from "$lib/i18n";

  export let value = "";
  let expanded = false;
  let host: HTMLDivElement;
  let view: { destroy: () => void } | null = null;

  async function mountEditor() {
    const { EditorState } = await import("@codemirror/state");
    const { EditorView, keymap } = await import("@codemirror/view");
    const { indentOnInput } = await import("@codemirror/language");
    const { defaultKeymap } = await import("@codemirror/commands");
    const shellLang = await (
      await import("$lib/editor/languages")
    ).shellLanguage();

    const theme = EditorView.theme({
      "&": {
        fontSize: "13px",
        backgroundColor: "transparent",
        border: "1px solid #3f3f46",
        borderRadius: "6px",
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
      },
      ".cm-content": {
        padding: "6px 8px",
        minHeight: "3.2em",
        caretColor: "#a5b4fc",
      },
      ".cm-line": { paddingLeft: "0" },
      "&.cm-focused": {
        outline: "none",
        border: "1px solid #52525b",
        boxShadow: "0 0 0 2px rgba(99, 102, 241, 0.35)",
      },
      ".cm-scroller": {
        fontFamily: "inherit",
        lineHeight: "1.45",
      },
    });

    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: value,
        extensions: [
          shellLang,
          indentOnInput(),
          keymap.of(defaultKeymap),
          EditorView.lineWrapping,
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              value = update.state.doc.toString();
            }
          }),
          theme,
        ],
      }),
    });
  }

  onMount(() => {
    if (expanded) void mountEditor();
  });

  onDestroy(() => {
    view?.destroy();
  });

  function toggleExpanded() {
    expanded = !expanded;
    // Remount the editor for the new state.
    view?.destroy();
    view = null;
    if (expanded) void mountEditor();
  }
</script>

<div class="startup-field">
  <div class="flex items-center justify-between">
    <span class="startup-label">{t($lang, "startup.label")}</span>
    <button
      type="button"
      class="rounded-md p-1 text-zinc-400 transition-colors hover:bg-zinc-700 hover:text-zinc-200"
      title={expanded
        ? t($lang, "startup.collapse")
        : t($lang, "startup.expand")}
      on:click={toggleExpanded}
    >
      {#if expanded}
        <ChevronsDownIcon size="14" />
      {:else}
        <ChevronsUpIcon size="14" />
      {/if}
    </button>
  </div>
  {#if expanded}
    <div class="overflow-hidden rounded-md leading-none" bind:this={host} />
  {:else}
    <input
      class="input-base font-mono text-xs text-zinc-200 placeholder:text-zinc-600"
      bind:value
      placeholder="export LANG=C; cd /data/app"
      title={t($lang, "startup.title")}
    />
  {/if}
  <p class="startup-desc">{t($lang, "startup.desc")}</p>
</div>

<style lang="postcss">
  .startup-field {
    @apply flex flex-col gap-1;
  }

  .startup-label {
    @apply text-sm font-medium text-zinc-200;
  }

  .startup-desc {
    @apply text-xs text-zinc-500;
  }
</style>
