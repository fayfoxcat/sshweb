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
  import type { IDecoration, IMarker, Terminal } from "sshx-xterm";
  import type { FitAddon } from "@xterm/addon-fit";
  import { Buffer } from "buffer";

  import themes from "./themes";
  import { settings } from "$lib/settings";
  import { copyText, readClipboard } from "$lib/clipboard";
  import { readDropPayload, type DropPayload } from "$lib/upload";
  import {
    COMMAND_TIME_GAP,
    commandTimeFor,
    isCommandLine,
  } from "$lib/cmdtime";
  import { DEFAULT_FONT_SIZE } from "$lib/constants";

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
        // The stamp is a DOM overlay with its own copy of the font, so it has to
        // be re-applied by hand (its box is resized by the renderer, its text
        // would otherwise stay at the old size).
        for (const { decoration } of stamps) {
          if (decoration.element) applyStampFont(decoration.element);
        }
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

  // ---- Command-submit timestamps (已知坑 99) ----
  // A command the user submits gets a dim gray time stamp right after it. The
  // stamp is an xterm **decoration** anchored to a marker on the command's row:
  // its text lives in a DOM overlay (`.xterm-decoration`, a child of
  // `.xterm-screen` and hence *outside* the `aria-hidden` `.xterm-rows`), never
  // in the buffer. That is what makes "不会被复制选中" structural rather than
  // cosmetic — `getSelection()` reads the buffer model, so the digits cannot be
  // in a copy, and the command's own bytes are untouched. The marker is
  // disposed once scrollback trims its line, and `DecorationService` disposes
  // the decoration along with it, so nothing drifts or leaks.

  /** Live stamps, so the settings switch can remove them at once and a font-size
   *  change can re-apply the font. Entries remove themselves on dispose. */
  const stamps: { marker: IMarker; decoration: IDecoration }[] = [];

  /** The stamp is positioned on the character grid, so it has to use the
   *  terminal's font rather than whatever the overlay would inherit. */
  function applyStampFont(el: HTMLElement) {
    if (!term) return;
    el.style.fontFamily = term.options.fontFamily ?? "";
    el.style.fontSize = `${term.options.fontSize ?? DEFAULT_FONT_SIZE}px`;
  }

  function clearStamps() {
    // `splice(0)` first: each `decoration.dispose()` fires the entry's own
    // dispose handler, which would otherwise mutate the array being iterated.
    for (const { marker, decoration } of stamps.splice(0)) {
      decoration.dispose();
      marker.dispose();
    }
  }

  /** Called from the key handler on a plain-Enter keydown, *before* xterm
   *  encodes it. Nothing here may change what is sent to the shell: no
   *  `preventDefault`, no `false` return, no write to the buffer. */
  function noteSubmit() {
    if (!term || !$settings.commandTime) return;
    const buf = term.buffer.active;
    // Full-screen programs draw on the alternate buffer (vim/less, and agent
    // TUIs such as claude/pi while they hold the screen): their lines are not
    // shell command lines and must never be stamped.
    if (buf.type !== "normal") return;
    // The logical line under the cursor: walk up past wrap continuations, then
    // read to the right edge (`translateToString(true)` drops the blanks the
    // shell left behind, which would otherwise hide the prompt's trailing
    // space). A shell that prints a prompt without a preceding newline (after
    // `printf 'x'`) leaves two candidate prompts on one line — the first is
    // still a prompt, so the judgement holds and only the cursor matters for
    // placement.
    const row = buf.baseY + buf.cursorY;
    let first = row;
    while (first > 0 && buf.getLine(first)?.isWrapped) first--;
    let line = "";
    for (let r = first; r <= row; r++) {
      line += buf.getLine(r)?.translateToString(true) ?? "";
    }
    if (!isCommandLine(line)) return;
    // Everything below is in **cells**, never string indices: a command with
    // CJK in it makes the two disagree (`cursorX` already counts 2 for one).
    const text = commandTimeFor(new Date(), buf.cursorX, term.cols);
    if (!text) return; // no room even for `HH:mm:ss` — a stamp may not cover the command
    const marker = term.registerMarker(0);
    if (!marker) return;
    const decoration = term.registerDecoration({
      marker,
      // Right after the command — the placement the first version used. Both
      // coordinates are in **cells** and are resolved by the renderer
      // (`left: x * cellW`), so the stamp keeps its distance from the command's
      // end when the pane is resized. `cols` never changes under a live
      // decoration's width, so a much later resize can still push the right end
      // of a stamp past the edge, which is only cosmetic (the digits are an
      // overlay; buffer and selection are untouched).
      x: buf.cursorX + COMMAND_TIME_GAP,
      width: text.length,
      height: 1,
      // Above the selection highlight (z-index 7 vs 6): the stamp stays readable
      // while text is being selected, which makes it obvious that it sits on
      // top of the row rather than being part of it.
      layer: "top",
    });
    if (!decoration) {
      marker.dispose();
      return;
    }
    const entry = { marker, decoration };
    // The element only exists while its row is on screen, and it is re-created
    // after a refresh, so the content and classes are set on every render.
    decoration.onRender((el) => {
      el.textContent = text;
      el.classList.add("cmd-time");
      el.setAttribute("aria-hidden", "true");
      applyStampFont(el);
    });
    decoration.onDispose(() => {
      const at = stamps.indexOf(entry);
      if (at >= 0) stamps.splice(at, 1);
      marker.dispose();
    });
    stamps.push(entry);
  }

  // Turning the switch off also removes the stamps already on screen, so the
  // setting has a visible effect instead of only applying to new commands.
  $effect(() => {
    const enabled = $settings.commandTime;
    if (ready && term && !enabled) clearStamps();
  });

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
      // `registerDecoration` (the command-time stamp, 已知坑 99) is a proposed
      // API: without this the call throws, and `registerMarker` — which is not
      // gated — would leave a marker with no stamp next to it.
      allowProposedApi: true,
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
      // Command-submit timestamps (已知坑 99). Only the keystroke that xterm
      // encodes as CR counts as a submit: Shift/Ctrl+Enter (LF) and Alt+Enter
      // (ESC+CR) are the newline escapes added for 已知坑 98, and stamping those
      // would put a stamp next to every line break inside a claude/pi prompt.
      // Purely additive — the keystroke is still handled by xterm below.
      if (
        event.type === "keydown" &&
        event.key === "Enter" &&
        !event.shiftKey &&
        !event.ctrlKey &&
        !event.altKey &&
        !event.isComposing
      ) {
        noteSubmit();
      }

      // Shift/Ctrl+Enter = newline, not submit (已知坑 98). The fork encodes
      // Enter, Shift+Enter and Ctrl+Enter as the *same* CR (see `case 13` in
      // `node_modules/sshx-xterm/src/common/input/Keyboard.ts`, which only ever
      // looks at `altKey`), and agent CLIs treat CR as "submit" — so without
      // this branch Shift+Enter cannot produce a line break in claude/pi at
      // all. Answer with LF (0x0A, the byte Ctrl+J sends) instead: it is
      // `chat:newline` in Claude Code and one of `tui.input.newLine`'s defaults
      // in pi, and in a plain shell readline binds LF and CR to the same
      // `accept-line`, so nothing changes there (vim treats LF as a line break
      // too).
      //
      // Three details are load-bearing: `keydown` (xterm consults this callback
      // for keypress/keyup as well, and Enter fires all three), `preventDefault`
      // (otherwise the browser still sends a keypress that xterm encodes into a
      // second, submitting CR) and `!isComposing` (an IME commits a candidate
      // with Enter; that keystroke must stay with the IME).
      if (
        event.type === "keydown" &&
        event.key === "Enter" &&
        (event.shiftKey || event.ctrlKey) &&
        !event.altKey &&
        !event.metaKey &&
        !event.isComposing
      ) {
        event.preventDefault();
        dispatch("data", new Uint8Array([0x0a]));
        return false;
      }

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

<style>
  /* Command-submit time stamp (已知坑 99). xterm creates the element while
     rendering, outside Svelte's own tree, so the rule has to be `:global`.
     No background: the stamp is plain dim text, so the command's own line looks
     untouched and the digits only read as an annotation. The gray is fixed
     rather than theme-derived because every shipped theme is dark, and the
     alpha keeps it clearly behind the terminal's own `#d8d8d8`-ish foreground
     (`#bcbcc4` at full opacity was still a touch loud; the hue is the same one
     the user settled on, only the alpha changed — 70% first, then 30%).
     `pointer-events: none` keeps the stamp out of the mouse path (selecting text
     and scrolling stay with the terminal underneath), and `user-select: none`
     means a native drag can never pick the digits up — the copy path could not
     see them anyway, since it reads the buffer model. */
  :global(.xterm .xterm-decoration.cmd-time) {
    color: rgba(188, 188, 196, 0.3);
    white-space: nowrap;
    pointer-events: none;
    user-select: none;
  }
</style>
