<!-- @component Pure CodeMirror 6 wrapper.
  Owns editor lifecycle, language selection, line diff decorations, cursor
  tracking, and search/undo/redo. The surrounding EditorWindow handles the
  chrome (title bar, encoding, save, window drag/resize).
-->
<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { browser } from "$app/environment";

  import { tr } from "$lib/i18n";
  import { languageForPath } from "$lib/editor/languages";
  import { applyDocChanges, type DiffStatus } from "$lib/editor/diff";

  export let filePath = "";
  export let lightMode = false;
  /** Called on doc change with the current edited/dirty state. */
  export let onEditedChange: (dirty: boolean) => void = () => {};
  /** Called when the cursor moves. */
  export let onCursorChange: (line: number, col: number) => void = () => {};
  /** Called when the search/replace panel opens or closes (drives the
   *  toolbar search button's active state). */
  export let onSearchOpenChange: (open: boolean) => void = () => {};

  // CodeMirror internals.
  let view: any = null;
  let containerEl: HTMLDivElement;

  /** Document text (also the pending doc until the view is created). */
  let docText = "";
  /** Snapshot of the original document for diffing. */
  let originalText = docText;
  /** Per-line diff flags keyed by current-doc line numbers (incremental). */
  let lineFlags: Map<number, DiffStatus> = new Map();

  // Diff decoration state (module refs set during setup).
  let Decoration: any;
  let RangeSet: any;
  let updateDecorations: any;
  let RangeSetBuilder: any;
  let undoCmd: any;
  let redoCmd: any;
  let openSearchPanelCmd: any;
  /** The `@codemirror/search` module (set during setup; used by toggle). */
  let searchMod: any = null;
  /** Whether the search/replace panel is currently open. */
  let searchOpen = false;

  // ---- Public API --------------------------------------------------------
  /** Normalize line endings to `\n`. CodeMirror 6 normalizes CRLF/CR to LF
   *  internally (DefaultSplit), so the diff baseline must match, otherwise
   *  the unedited file would be treated as fully changed. */
  function normalizeText(text: string): string {
    return text.replace(/\r\n?/g, "\n");
  }

  export function getDoc(): string {
    return view ? view.state.doc.toString() : docText;
  }

  /** Replace the document content, resetting the diff baseline to the new
   *  text (a fresh load from disk). */
  export function setDoc(text: string) {
    applyText(text, true);
  }

  /** Replace the document content, keeping the diff baseline intact. Used to
   *  restore an unsaved draft over freshly-loaded disk content, so the diff
   *  still shows the draft against the on-disk version. */
  export function replaceDoc(text: string) {
    applyText(text, false);
  }

  /** Apply `text` to the document. `resetBaseline` also resets the diff
   *  baseline; see [`Self::setDoc`] / [`Self::replaceDoc`]. */
  function applyText(text: string, resetBaseline: boolean) {
    text = normalizeText(text);
    docText = text;
    if (resetBaseline) {
      originalText = text;
      lineFlags = new Map();
    }
    if (view) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: text },
      });
      if (!lightMode) refreshDecorations();
    }
  }

  /** Update the diff baseline (on-disk content) without touching the doc. */
  export function setBaseline(text: string) {
    originalText = normalizeText(text);
    lineFlags = new Map();
    if (!lightMode) refreshDecorations();
  }

  export function undo() {
    view?.focus();
    undoCmd?.(view);
  }

  export function redo() {
    view?.focus();
    redoCmd?.(view);
  }

  /** Toggle the search/replace panel on and off. */
  export function toggleSearch() {
    view?.focus();
    if (view && searchMod) {
      if (searchMod.searchPanelOpen(view.state))
        searchMod.closeSearchPanel(view);
      else openSearchPanelCmd?.(view);
    }
  }

  // ---- Diff decorations --------------------------------------------------
  function refreshDecorations() {
    if (!view || !Decoration || lightMode) return;
    const builder = new RangeSetBuilder();
    const total = view.state.doc.lines;
    for (let line = 1; line <= total; line++) {
      const status = lineFlags.get(line);
      if (!status) continue;
      const from = view.state.doc.line(line).from;
      builder.add(from, from, Decoration.line({ class: `cm-diff-${status}` }));
    }
    view.dispatch({ effects: updateDecorations.of(builder.finish()) });
  }

  // ---- Setup -------------------------------------------------------------
  async function setup() {
    const viewMod = await import("@codemirror/view");
    const {
      EditorView,
      lineNumbers,
      highlightActiveLine,
      keymap,
      drawSelection,
    } = viewMod;
    Decoration = viewMod.Decoration;
    const stateMod = await import("@codemirror/state");
    const { EditorState, StateEffect, StateField } = stateMod;
    RangeSet = stateMod.RangeSet;
    RangeSetBuilder = stateMod.RangeSetBuilder;
    const { history, defaultKeymap, historyKeymap, undo, redo } = await import(
      "@codemirror/commands"
    );
    const searchMod_ = await import("@codemirror/search");
    searchMod = searchMod_;
    const {
      search,
      searchKeymap,
      openSearchPanel,
      findNext,
      findPrevious,
      replaceNext,
      replaceAll,
      setSearchQuery,
      getSearchQuery,
      closeSearchPanel,
      searchPanelOpen,
    } = searchMod_;
    const { syntaxHighlighting, defaultHighlightStyle } = await import(
      "@codemirror/language"
    );

    undoCmd = undo;
    redoCmd = redo;
    openSearchPanelCmd = openSearchPanel;
    updateDecorations = StateEffect.define();

    /** Dark, on-theme search/replace panel (the default CodeMirror panel uses
     *  light input styling that clashes with the rest of the app). */
    function createSearchPanel(view: any) {
      const dom = document.createElement("div");
      dom.className = "sshweb-search-panel";

      const searchInput = document.createElement("input");
      searchInput.setAttribute("main-field", "true");
      searchInput.type = "text";
      searchInput.placeholder = tr("editor.findPlaceholder");
      searchInput.value = getSearchQuery(view.state).search;
      searchInput.addEventListener("input", () => {
        view.dispatch({
          effects: setSearchQuery.of(
            new searchMod.SearchQuery({
              search: searchInput.value,
              caseSensitive: caseBtn.classList.contains("active"),
              regexp: regexBtn.classList.contains("active"),
            }),
          ),
        });
      });
      searchInput.addEventListener("keydown", (e: KeyboardEvent) => {
        if (e.key === "Enter") {
          e.preventDefault();
          findNext(view);
        } else if (e.key === "Escape") {
          e.preventDefault();
          closeSearchPanel(view);
        }
      });

      const caseBtn = makeToggleBtn(tr("editor.caseSensitive"), "Aa");
      const regexBtn = makeToggleBtn(tr("editor.regexp"), ".*");
      caseBtn.addEventListener("click", () => {
        caseBtn.classList.toggle("active");
        searchInput.dispatchEvent(new Event("input"));
      });
      regexBtn.addEventListener("click", () => {
        regexBtn.classList.toggle("active");
        searchInput.dispatchEvent(new Event("input"));
      });

      const replaceRow = document.createElement("div");
      replaceRow.className = "sshweb-search-row";
      const replaceInput = document.createElement("input");
      replaceInput.type = "text";
      replaceInput.placeholder = tr("editor.replacePlaceholder");
      replaceInput.addEventListener("input", () => {
        view.dispatch({
          effects: setSearchQuery.of(
            new searchMod.SearchQuery({
              search: searchInput.value,
              replace: replaceInput.value,
              caseSensitive: caseBtn.classList.contains("active"),
              regexp: regexBtn.classList.contains("active"),
            }),
          ),
        });
      });
      const replaceOne = makeActionBtn(tr("editor.replaceOne"), () =>
        replaceNext(view),
      );
      const replaceAllBtn = makeActionBtn(tr("editor.replaceAll"), () =>
        replaceAll(view),
      );

      // Prev/Next buttons.
      const prevBtn = makeActionBtn("↑", () => findPrevious(view));
      const nextBtn = makeActionBtn("↓", () => findNext(view));

      const searchRow = document.createElement("div");
      searchRow.className = "sshweb-search-row";
      searchRow.append(searchInput, caseBtn, regexBtn, prevBtn, nextBtn);
      replaceRow.append(replaceInput, replaceOne, replaceAllBtn);
      dom.append(searchRow, replaceRow);

      return {
        dom,
        mount() {
          // CodeMirror calls this once the panel is attached to the editor:
          // focus (and select) the search field so typing goes straight into
          // the query instead of staying in the editor. The default panel does
          // the same via its own `mount`.
          searchInput.focus();
          searchInput.select();
        },
        update(update: any) {
          if (!update.docChanged && !update.selectionSet) return;
          const q = getSearchQuery(view.state);
          searchInput.value = q.search;
          replaceInput.value = q.replace;
        },
        destroy() {
          // The panel is removed by the library; nothing to clean up here.
        },
      } as unknown as any;

      function makeToggleBtn(title: string, label: string) {
        const b = document.createElement("button");
        b.className = "sshweb-search-toggle";
        b.textContent = label;
        b.title = title;
        return b;
      }

      function makeActionBtn(title: string, onClick: () => void) {
        const b = document.createElement("button");
        b.className = "sshweb-search-action";
        b.textContent = title;
        b.title = title;
        b.addEventListener("click", (e: MouseEvent) => {
          e.preventDefault();
          onClick();
        });
        return b;
      }
    }

    // Language support (only in normal mode).
    let langExt: any = [];
    if (!lightMode) {
      const lang = await languageForPath(filePath);
      if (lang) langExt = [lang];
    }

    // Line diff decorations via a state field.
    const diffField = StateField.define({
      create: () => RangeSet.empty,
      update(value: any, tr: any) {
        value = value.map(tr.changes);
        for (const e of tr.effects) {
          if (e.is(updateDecorations)) value = e.value;
        }
        return value;
      },
      provide: (f: any) => EditorView.decorations.from(f),
    });

    // Track cursor + dirty state (and the search panel open/close state).
    const updateListener = EditorView.updateListener.of((update: any) => {
      const open = searchPanelOpen(update.state);
      if (open !== searchOpen) {
        searchOpen = open;
        onSearchOpenChange(open);
      }
      if (update.selectionSet || update.docChanged) {
        if (update.docChanged) {
          // Incrementally update per-line diff flags from this exact change.
          lineFlags = applyDocChanges(
            update.startState.doc,
            update.state.doc,
            update.changes,
            lineFlags,
          );
          const dirty = update.state.doc.toString() !== originalText;
          // Back to the on-disk content (e.g. undo) -> nothing is modified.
          if (!dirty) lineFlags = new Map();
          onEditedChange(dirty);
          if (!lightMode) refreshDecorations();
        } else {
          const head = update.state.selection.main.head;
          const line = update.state.doc.lineAt(head);
          onCursorChange(line.number, head - line.from + 1);
        }
      }
    });

    const extensions: any[] = [
      lineNumbers(),
      history(),
      drawSelection(),
      keymap.of([...defaultKeymap, ...historyKeymap]),
      updateListener,
    ];

    if (!lightMode) {
      extensions.push(
        keymap.of(searchKeymap),
        highlightActiveLine(),
        // Fallback theme for legacy StreamLanguage grammars (shell/nginx/
        // properties); modern lang-* packages bring their own highlighting.
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        diffField,
      );
    }

    const state = EditorState.create({
      doc: docText,
      extensions: [
        ...extensions,
        search({ top: true, createPanel: createSearchPanel }),
        ...langExt,
      ],
    });

    view = new EditorView({ state, parent: containerEl });

    // Initial diff decorations (skipped in light mode).
    if (!lightMode) refreshDecorations();
  }

  onMount(() => {
    if (browser) setup();
  });

  onDestroy(() => {
    view?.destroy();
  });
</script>

<div class="h-full w-full" bind:this={containerEl} />

<svelte:head>
  <style>
    .cm-editor {
      height: 100%;
      font-size: 13px;
      background: #111 !important;
      color: #e4e4e7;
    }
    .cm-editor .cm-content {
      font-family: "Fira Code VF", ui-monospace, monospace;
    }
    .cm-editor.cm-focused {
      outline: none;
    }
    .cm-editor .cm-cursor,
    .cm-editor .cm-dropCursor {
      border-left: 2px solid #e4e4e7 !important;
    }
    .cm-editor .cm-cursorLayer {
      animation: none !important;
    }
    .cm-editor .cm-selectionBackground {
      background: rgba(99, 102, 241, 0.3) !important;
    }
    .cm-gutters {
      background: #161616 !important;
      color: #52525b;
      border-right: 1px solid #71717a !important;
    }
    .cm-editor .cm-lineNumbers .cm-gutterElement {
      min-width: 40px;
      padding: 0 16px 0 12px;
      text-align: right;
    }
    .cm-activeLine {
      background: rgba(255, 255, 255, 0.03) !important;
    }
    .cm-activeLineGutter {
      background: rgba(255, 255, 255, 0.04) !important;
    }
    .cm-diff-added {
      box-shadow: inset 3px 0 0 rgba(34, 197, 94, 0.8);
    }
    .cm-diff-removed {
      box-shadow: inset 3px 0 0 rgba(113, 113, 122, 0.8);
    }
    .cm-diff-modified {
      box-shadow: inset 3px 0 0 rgba(59, 130, 246, 0.8);
    }
    /* Custom dark search/replace panel (matches the app theme). Kill the
       default CodeMirror panel border (the visible white line) too. */
    .cm-panels-top,
    .cm-panels-bottom {
      border: 0 !important;
    }
    .sshweb-search-panel {
      display: flex;
      flex-direction: column;
      gap: 6px;
      padding: 8px;
      background: #161616;
    }
    .sshweb-search-row {
      display: flex;
      align-items: center;
      gap: 6px;
    }
    .sshweb-search-panel input[type="text"] {
      flex: 1;
      min-width: 0;
      background: #1c1c1c;
      color: #e4e4e7;
      border: 1px solid #52525b;
      border-radius: 4px;
      padding: 4px 8px;
      font-size: 12px;
      outline: none;
    }
    .sshweb-search-panel input[type="text"]:focus {
      border-color: #818cf8;
    }
    .sshweb-search-panel button {
      background: #262626;
      color: #a1a1aa;
      border: 1px solid #3f3f46;
      border-radius: 4px;
      padding: 3px 8px;
      font-size: 12px;
      line-height: 1;
      cursor: pointer;
    }
    .sshweb-search-panel button:hover {
      background: #3f3f46;
      color: #e4e4e7;
    }
    .sshweb-search-panel button.sshweb-search-toggle.active {
      background: #4f46e5;
      color: #fff;
      border-color: #4f46e5;
    }
  </style>
</svelte:head>
