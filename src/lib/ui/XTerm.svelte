<!-- @component Interactive terminal rendered with xterm.js -->
<script lang="ts" context="module">
  import { tr } from "$lib/i18n";
  import { makeToast } from "$lib/toast";

  // Deduplicated terminal font loading.
  const waitForFonts = (() => {
    let state: "initial" | "loading" | "loaded" = "initial";
    const waitlist: (() => void)[] = [];

    return async function waitForFonts() {
      if (state === "loaded") return;
      else if (state === "initial") {
        const FontFaceObserver = (await import("fontfaceobserver")).default;
        state = "loading";
        try {
          await new FontFaceObserver("Fira Code VF").load();
        } catch (error) {
          makeToast({
            kind: "error",
            message: tr("xterm.fontError"),
          });
        }
        state = "loaded";
        for (const fn of waitlist) fn();
      } else {
        await new Promise<void>((resolve) => {
          if (state === "loaded") resolve();
          else waitlist.push(resolve);
        });
      }
    };
  })();
</script>

<script lang="ts">
  import { browser } from "$app/environment";

  import { createEventDispatcher, onDestroy, onMount } from "svelte";
  import { debounce } from "lodash-es";
  import type { Terminal } from "sshx-xterm";
  import type { FitAddon } from "xterm-addon-fit";
  import { Buffer } from "buffer";

  import themes from "./themes";
  import { settings } from "$lib/settings";
  import { copyText, readClipboard } from "$lib/clipboard";
  import { readDropPayload, type DropPayload } from "$lib/upload";

  /** Used to determine Cmd versus Ctrl keyboard shortcuts. */
  const isMac = browser && navigator.platform.startsWith("Mac");

  const dispatch = createEventDispatcher<{
    data: Uint8Array;
    resize: { rows: number; cols: number };
    dropfiles: { payload: DropPayload };
  }>();

  export let rows: number, cols: number;
  export let write: (data: string) => void; // bound function prop
  export let active: boolean; // whether this terminal is the visible tab

  export let title: string = "Terminal"; // bound prop, updated on title change
  let element: HTMLDivElement;
  let term: Terminal | null = null;
  let fitAddon: FitAddon | null = null;

  $: theme = themes[$settings.theme];

  $: if (term) {
    // If the theme changes, update existing terminals' appearance.
    term.options.theme = theme;
    term.options.scrollback = $settings.scrollback;
  }

  let loaded = false;

  /** Resize the terminal to fill its container, then notify the server. */
  async function fit() {
    if (!term || !fitAddon) return;
    try {
      fitAddon.fit();
      term.refresh(0, term.rows - 1);
      dispatch("resize", { rows: term.rows, cols: term.cols });
    } catch (err) {
      // The container has zero dimensions; try again once it is visible.
    }
  }

  // When this tab becomes active, re-fit now that it is visible and claim
  // keyboard focus so typing / Ctrl+C / Ctrl+V reach xterm immediately
  // (xterm only receives keys while its hidden textarea is focused).
  function claimFocus() {
    setTimeout(() => {
      fit();
      term?.focus();
    }, 0);
  }

  $: if (term && active) {
    claimFocus();
  }

  onMount(() => {
    const onResize = debounce(() => fit(), 150);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  });

  onDestroy(() => term?.dispose());

  const preloadBuffer: string[] = [];

  write = (data: string) => {
    if (!term) {
      // Before the terminal is loaded, push data into a buffer.
      preloadBuffer.push(data);
    } else {
      term.write(data);
    }
  };

  // ---- Clipboard ----
  // Copy is implemented here: the fork's own `copy` handler listens on the
  // terminal element and only fires when the browser emits a DOM `copy` event,
  // which the canvas-drawn selection often doesn't trigger. Paste is handled
  // natively by the fork (`handlePasteEvent` on the textarea `paste` event) —
  // adding our own paste listener would send the text twice.
  const utf8Encoder = new TextEncoder();

  /** Copy the terminal selection to the OS clipboard (async Clipboard API
   *  with the legacy textarea fallback; see `lib/clipboard.ts`). */
  function copySelection() {
    const text = term?.getSelection();
    if (!text) return;
    void copyText(text);
  }

  /** Right-click paste: read the OS clipboard and feed it through the fork's
   *  own `paste()` (bracketed-paste aware, same as Ctrl+V). An empty read is a
   *  legitimately empty clipboard (nothing to paste, no error); a failed read
   *  reports the cause — the page is not a secure context (plain HTTP from a
   *  LAN origin blocks clipboard reads outright) vs. the browser denied the
   *  read permission. */
  async function pasteOnRightClick() {
    const result = await readClipboard();
    if (result.ok) {
      if (result.text) term?.paste(result.text);
      return;
    }
    makeToast({
      kind: "error",
      message:
        result.reason === "insecure"
          ? tr("xterm.rightClickPasteInsecure")
          : tr("xterm.rightClickPasteDenied"),
    });
  }

  onMount(async () => {
    const [{ Terminal }, { WebLinksAddon }, { ImageAddon }, { FitAddon }] =
      await Promise.all([
        import("sshx-xterm"),
        import("xterm-addon-web-links"),
        import("xterm-addon-image"),
        import("xterm-addon-fit"),
      ]);

    await waitForFonts();

    term = new Terminal({
      allowTransparency: false,
      cursorBlink: false,
      cursorStyle: "block",
      // Monospace fonts, with CJK-capable monospace faces first so Chinese /
      // Korean / Japanese output renders at exactly the 2-column width xterm
      // expects. Without a matching CJK glyph, the browser falls back to a
      // proportional font and readline's prompt redraws drift (arrow-key
      // "漂移"). The WebGL renderer is deliberately NOT used: its glyph
      // measurement is unreliable for double-width CJK in a fallback font,
      // whereas the default canvas renderer lays cells out on a fixed grid,
      // so cursor position never drifts.
      fontFamily:
        '"Fira Code VF", "Sarasa Mono SC", "Noto Sans Mono CJK SC", "WenQuanYi Zen Hei Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace',
      fontSize: 14,
      fontWeight: 400,
      fontWeightBold: 500,
      lineHeight: 1.06,
      scrollback: $settings.scrollback,
      theme,
    });

    // Copy / paste + natural text-editing shortcuts.
    //   - Ctrl/Cmd+C with a selection  → copy the selection (no ^C);
    //   - Ctrl/Cmd+C without selection → send ^C (SIGINT) to the shell;
    //   - Ctrl/Cmd+V                   → paste. Returning false stops xterm
    //     from sending ^V; the browser's default paste then fires the
    //     `paste` event on the textarea, which the fork handles natively
    //     (`handlePasteEvent`), delivering the clipboard text exactly once.
    term.attachCustomKeyEventHandler((event) => {
      const primary = isMac
        ? event.metaKey && !event.ctrlKey && !event.altKey
        : event.ctrlKey && !event.metaKey && !event.altKey;

      if (primary) {
        if (event.key === "c" || event.key === "C") {
          if (term?.hasSelection()) {
            copySelection();
            return false;
          }
          return true; // no selection: Ctrl+C = SIGINT
        }
        if (event.key === "v" || event.key === "V") {
          return false; // let the native paste event fire on the textarea
        }
        if (event.shiftKey) {
          if (event.key === "v" || event.key === "V") {
            return false;
          }
          // Ctrl+Shift+C is a browser-level shortcut; leave it alone.
        }
        if (event.key === "ArrowLeft") {
          dispatch("data", new Uint8Array([0x01]));
          return false;
        } else if (event.key === "ArrowRight") {
          dispatch("data", new Uint8Array([0x05]));
          return false;
        } else if (event.key === "Backspace") {
          dispatch("data", new Uint8Array([0x15]));
          return false;
        }
      }
      return true;
    });

    term.loadAddon(new WebLinksAddon());
    term.loadAddon(new ImageAddon({ enableSizeReports: false }));

    fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    term.open(element);

    // Paste is handled by the fork's own `paste` listener on the textarea
    // (reads the clipboard, applies bracketed-paste mode and sends it). We do
    // NOT add our own paste listener — it would send the text twice.

    // Right-click pastes the clipboard into the terminal (like Ctrl+V), which
    // is the expected behaviour on Linux-style terminals.
    term.element?.addEventListener("contextmenu", (event: MouseEvent) => {
      event.preventDefault();
      term?.focus();
      void pasteOnRightClick();
    });

    term.resize(cols, rows);
    term.onTitleChange((newTitle) => {
      title = newTitle;
    });

    loaded = true;
    for (const data of preloadBuffer) {
      term.write(data);
    }

    term.onData((data: string) => {
      dispatch("data", utf8Encoder.encode(data));
    });
    term.onBinary((data: string) => {
      dispatch("data", Buffer.from(data, "binary"));
    });
  });
</script>

<div
  class="h-full w-full"
  bind:this={element}
  style:background={theme.background}
  style:opacity={loaded ? 1.0 : 0.0}
  on:dragover|preventDefault={(event) => {
    if (event.dataTransfer?.types?.includes("Files")) {
      event.dataTransfer.dropEffect = "copy";
    }
  }}
  on:drop|preventDefault={(event) => {
    // Local files/folders dropped onto the terminal: snapshot the entries
    // synchronously (webkitGetAsEntry — dataTransfer.files loses folder
    // structure) and forward to the parent, which queries the shell's pwd
    // and uploads there.
    if (!event.dataTransfer) return;
    const payload = readDropPayload(event.dataTransfer);
    if (payload.entries.length === 0 && payload.files.length === 0) return;
    dispatch("dropfiles", { payload });
  }}
/>
