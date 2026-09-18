<!-- @component Interactive terminal rendered with xterm.js -->
<script module lang="ts">
  import { tr } from "$lib/i18n";
  import { makeToast } from "$lib/toast";

  // Deduplicated terminal font loading.
  const waitForFonts = (() => {
    let state: "initial" | "loading" | "loaded" = "initial";
    const waitlist: (() => void)[] = [];

    return async function waitForFonts(fontSize: number) {
      if (state === "loaded") return;
      else if (state === "initial") {
        state = "loading";
        try {
          // `document.fonts` is Baseline Widely available (Chrome 35+), and
          // `app.css` declares the face itself, so the third-party observer is
          // gone. One semantic difference: `load()` *resolves* with an empty
          // list when the family isn't in the FontFaceSet (it only rejects on
          // an actual fetch failure), so an empty result is what maps to
          // fontfaceobserver's "font unavailable" and keeps the toast.
          //
          // The size is the configured one (only the family decides whether a
          // face matches, but there is no reason to probe at a size the user
          // does not use).
          const loaded = await document.fonts.load(
            `${fontSize}px "Fira Code VF"`,
          );
          if (loaded.length === 0) throw new Error("font unavailable");
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
  import type { Terminal } from "sshx-xterm";
  import type { FitAddon } from "@xterm/addon-fit";
  import { Buffer } from "buffer";

  import themes from "./themes";
  import { settings } from "$lib/settings";
  import { copyText, readClipboard } from "$lib/clipboard";
  import { readDropPayload, type DropPayload } from "$lib/upload";

  const dispatch = createEventDispatcher<{
    data: Uint8Array;
    resize: { rows: number; cols: number };
    dropfiles: { payload: DropPayload };
  }>();

  interface Props {
    rows: number;
    cols: number;
    /** Bound by the parent: the shell's chunk writer. */
    write?: (
      data: string,
    ) => void; /** Whether this terminal is the visible tab. */
    active: boolean;
  }

  let {
    rows,
    cols,
    write = $bindable<(data: string) => void>(),
    active,
  }: Props = $props();

  /** Used to determine Cmd versus Ctrl keyboard shortcuts. */
  const isMac = browser && navigator.platform.startsWith("Mac");

  /** Minimal trailing debounce — replaces `lodash-es` `debounce`, the app's
   *  only lodash use, so the vulnerable lodash-es bundle is never shipped. */
  function debounce<A extends unknown[]>(
    fn: (...args: A) => void,
    wait: number,
  ) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    return (...args: A): void => {
      clearTimeout(timer);
      timer = setTimeout(() => fn(...args), wait);
    };
  }

  let element = $state<HTMLDivElement>();
  let term: Terminal | null = null;
  let fitAddon: FitAddon | null = null;

  /** Whether `term` has been created. `term` itself is a plain `let`, so it is
   *  invisible to `$effect` — this flag is the reactive signal that says "the
   *  terminal now exists", and it is what makes the focus effect below fire
   *  for a terminal that mounts already active (a new tab, or the restored
   *  terminals after a page reload). Without it the effect only ever ran on an
   *  `active` transition, which never happens for a freshly opened tab that is
   *  active from the start — so its keys went nowhere until the user clicked. */
  let ready = $state(false);

  let theme = $derived(themes[$settings.theme]);
  let loaded = $state(false);

  // When the theme / scrollback / font-size settings change, apply them to
  // existing terminals' appearance. The font size is tracked separately because
  // it is the only one that changes the *geometry*: the character cell is
  // re-measured (xterm measures on every `fontSize` write), so the row/column
  // count no longer fills the pane and the server's PTY keeps the old winsize —
  // which makes the remote wrap at the wrong column. Hence the re-`fit()`,
  // which recomputes rows/cols and reports them.
  //
  // The settings are read *before* the `if`, and the guard is `ready` rather
  // than `term`, for the same reason as the focus effect above: `term` is a
  // plain `let`, and this effect's first run happens before the terminal
  // exists (`onMount` awaits the xterm imports). Reading them inside the guard
  // meant the first run touched no reactive value at all, so the effect
  // subscribed to nothing and never ran again — the settings then only ever
  // applied to terminals created *after* the change.
  let appliedFontSize = $settings.fontSize;
  $effect(() => {
    const nextTheme = theme;
    const nextScrollback = $settings.scrollback;
    const nextFontSize = $settings.fontSize;
    if (ready && term) {
      term.options.theme = nextTheme;
      term.options.scrollback = nextScrollback;
      if (nextFontSize !== appliedFontSize) {
        appliedFontSize = nextFontSize;
        term.options.fontSize = nextFontSize;
        void fit();
      }
    }
  });

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

  // When this tab becomes active — or when an already-active tab's terminal
  // finishes loading — re-fit now that it is visible and claim keyboard focus
  // so typing / Ctrl+C / Ctrl+V reach xterm immediately (xterm only receives
  // keys while its hidden textarea is focused). `ready` is what covers the
  // second case: a tab opened as active is mounted with `active === true`
  // before `term` exists, so the `active` edge alone would never fire.
  function claimFocus() {
    setTimeout(() => {
      fit();
      term?.focus();
    }, 0);
  }

  $effect(() => {
    if (ready && active) claimFocus();
  });

  onMount(() => {
    const onResize = debounce(() => fit(), 150);
    window.addEventListener("resize", onResize);
    // The pane also changes size with no window resize involved: the editor tab
    // bar appears/leaves below the terminal, and the SFTP sidebar is dragged
    // wider or narrower. xterm would keep its old row/column count in that
    // case, and because the pane clips (`overflow-hidden`) the bottom rows —
    // the prompt, i.e. whatever is being typed — end up hidden behind the tab
    // bar. Observing the element itself covers every container-driven change.
    // Only the active shell's fit reaches the server (`handleResize`), so
    // refitting the hidden terminals too is harmless.
    const observer = new ResizeObserver(onResize);
    observer.observe(element!);
    return () => {
      window.removeEventListener("resize", onResize);
      observer.disconnect();
    };
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
   *  own `paste()` (bracketed-paste aware, same as Ctrl+V). */
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
    // The scoped `@xterm/addon-*` packages are pinned to their last build of
    // the **5.x generation** (see package.json): the newest releases target
    // `@xterm/xterm@6`, which dropped the canvas renderer this component
    // depends on (see the `fontFamily` note below). The unscoped
    // `xterm-addon-*` names these replace are npm-deprecated.
    const [{ Terminal }, { WebLinksAddon }, { ImageAddon }, { FitAddon }] =
      await Promise.all([
        import("sshx-xterm"),
        import("@xterm/addon-web-links"),
        import("@xterm/addon-image"),
        import("@xterm/addon-fit"),
      ]);

    await waitForFonts($settings.fontSize);

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
      fontSize: $settings.fontSize,
      fontWeight: 400,
      fontWeightBold: 500,
      lineHeight: 1.06,
      scrollback: $settings.scrollback,
      theme,
    });

    // Copy / paste + natural text-editing shortcuts.
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

    term.open(element!);

    // Paste is handled by the fork's own `paste` listener on the textarea
    // (reads the clipboard, applies bracketed-paste mode and sends it). We do
    // NOT add our own paste listener — it would send the text twice.

    // Right-click pastes the clipboard into the terminal (like Ctrl+V).
    term.element?.addEventListener("contextmenu", (event: MouseEvent) => {
      event.preventDefault();
      term?.focus();
      void pasteOnRightClick();
    });

    term.resize(cols, rows);

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

    // Announce readiness last, so the focus effect sees a fully set-up
    // terminal (`ready` gates nothing else; it is purely the effect's signal).
    ready = true;
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="h-full w-full"
  bind:this={element}
  style:background={theme.background}
  style:opacity={loaded ? 1.0 : 0.0}
  ondragover={(event) => {
    event.preventDefault();
    if (event.dataTransfer?.types?.includes("Files")) {
      event.dataTransfer.dropEffect = "copy";
    }
  }}
  ondrop={(event) => {
    event.preventDefault();
    // Local files/folders dropped onto the terminal: snapshot the entries
    // synchronously (webkitGetAsEntry — dataTransfer.files loses folder
    // structure) and forward to the parent, which queries the shell's pwd
    // and uploads there.
    if (!event.dataTransfer) return;
    const payload = readDropPayload(event.dataTransfer);
    if (payload.entries.length === 0 && payload.files.length === 0) return;
    dispatch("dropfiles", { payload });
  }}
></div>
