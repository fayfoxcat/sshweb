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
  import type { IBuffer, IDecoration, IMarker, Terminal } from "sshx-xterm";
  import type { FitAddon } from "@xterm/addon-fit";
  import { Buffer } from "buffer";

  import themes from "./themes";
  import { settings } from "$lib/settings";
  import { copyText, readClipboard } from "$lib/clipboard";
  import { readDropPayload, type DropPayload } from "$lib/upload";
  import {
    COMMAND_TIME_GAP,
    commandOnLine,
    commandTimeFor,
    isCommandLine,
  } from "$lib/cmdtime";
  import { DEFAULT_FONT_SIZE } from "$lib/constants";

  const dispatch = createEventDispatcher<{
    data: Uint8Array;
    resize: { rows: number; cols: number };
    dropfiles: { payload: DropPayload };
    /** A command was submitted with a plain Enter — the text the shell is about
     *  to run, for the 「命令片段」 sidebar's history mode (`snippets.ts`). Not
     *  emitted for Shift/Ctrl+Enter or Alt+Enter, which are the newline
     *  escapes, and not on the alternate screen (vim/less/claude). */
    submit: string;
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

  /** Put the keyboard back into this terminal.
   *
   *  Used after the 「命令片段」 sidebar types a command in: the click landed on
   *  the sidebar, so xterm's hidden textarea no longer has focus and the user
   *  would otherwise have to click the terminal before pressing Enter. Safe to
   *  call before the terminal exists (`term` is null then; the `ready` effect
   *  focuses it once it does). */
  export function focus() {
    term?.focus();
  }

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

  /** Column just past the last non-blank **cell** of `row` in `buf`, or 0 for a
   *  blank row. Counted in cells (a CJK glyph is 2 wide), never in string
   *  indices — see 坑 108: the stamp has to sit after the whole command, and the
   *  cell count is what the renderer and the shell's own cursor agree on. */
  function lineEndColumn(buf: IBuffer, row: number): number {
    const line = buf.getLine(row);
    if (!line) return 0;
    let end = 0;
    for (let x = 0; x < line.length; x++) {
      const cell = line.getCell(x);
      if (!cell) break;
      const width = cell.getWidth();
      // 0 = the second half of a wide glyph: part of the cell before it.
      if (width === 0) continue;
      if (cell.getChars().trim() !== "") end = x + width;
    }
    return end;
  }

  /** Absolute row of `buf`'s cursor (scrollback included). */
  function cursorRow(buf: IBuffer): number {
    return buf.baseY + buf.cursorY;
  }

  /** Render the stamp for the command whose **first** row is pinned by `anchor`
   *  in `buf` — the buffer that was active on the keydown. The marker was
   *  registered back then, so scrollback trimming while the echo is in flight
   *  moves it along with the line instead of leaving us measuring a neighbour.
   *  A wrapped command spans several rows and the stamp belongs on the last one,
   *  after that row's last non-blank cell. An impossible stamp (no room even for
   *  `HH:mm:ss`) is simply not drawn. */
  function renderStamp(buf: IBuffer, anchor: IMarker, at: Date) {
    if (!term) return;
    if (anchor.line < 0) return; // trimmed away while the echo was in flight
    // Walk down past the continuation rows of a wrapped command: the line the
    // shell executes ends on the last of them, and a long command wraps on a
    // narrow pane far more often than one might think.
    let last = anchor.line;
    while (buf.getLine(last + 1)?.isWrapped) last++;
    const endColumn = lineEndColumn(buf, last);
    const text = commandTimeFor(at, endColumn, term.cols);
    if (!text) {
      anchor.dispose();
      return;
    }
    let marker = anchor;
    if (last !== anchor.line) {
      // A marker's line is fixed at registration, so the wrapped case needs a
      // second one on the command's last row — and markers can only be
      // registered in the *active* buffer. If the screen moved on to the other
      // buffer in the meantime (a command that itself switches to the alternate
      // screen), the stamp would land on a foreign screen, so dropping it is
      // the honest outcome.
      if (term.buffer.active !== buf) {
        anchor.dispose();
        return;
      }
      const lastRow = term.registerMarker(last - cursorRow(buf));
      anchor.dispose();
      if (!lastRow) return;
      marker = lastRow;
    }
    const decoration = term.registerDecoration({
      marker,
      x: endColumn + COMMAND_TIME_GAP,
      width: text.length,
      height: 1,
      layer: "top",
    });
    if (!decoration) {
      marker.dispose();
      return;
    }
    const entry = { marker, decoration };
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

  /** Create the stamp for the command that was just submitted.
   *
   *  Deliberately **deferred until the shell has echoed the newline** (坑 108):
   *  at the keydown the echo of the last typed character may still be in
   *  flight (the keystroke and its echo travel different paths: browser →
   *  server → PTY → back), so the buffered line is a character or two short. A
   *  stamp placed from that stale position *lands on top of the command* as
   *  soon as the echo arrives — the digits then overlap the last characters,
   *  which is exactly what the user saw after recalling a long command with ↑
   *  and editing it.
   *
   *  The signal is the **cursor leaving the command's row**: the shell's echo of
   *  the Enter is a CR+LF, so once the cursor row moved past the command the
   *  line can no longer change. `onWriteParsed` fires at most once per frame
   *  after a batch has been parsed, which is exactly the granularity needed;
   *  the timeout is the fallback for anything that never echoes (a program that
   *  swallows the Enter, a shell redrawing the line elsewhere) — a stamp is
   *  never lost, only placed on whatever is there.
   *
   *  The buffer is captured here, not looked up when the stamp is drawn: a
   *  command may itself switch to the alternate screen (`vim`, `less`, an agent
   *  TUI), and the line to measure lives in the buffer the user typed on.
   *
   *  `at` is the moment the user pressed Enter: the digits say when the command
   *  ran, not when its echo happened to arrive. */
  function stampCommand(at: Date) {
    if (!term) return;
    const buf = term.buffer.active;
    // Registered now, while the cursor is still on the command line: the marker
    // follows that row through scrollback trimming, so measuring it later can
    // never drift onto a neighbour.
    const anchor = term.registerMarker(0);
    if (!anchor) return;
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let subscription: { dispose(): void } | null = null;
    const finalize = (force: boolean) => {
      if (settled) return;
      // Still on the command line of the same buffer: the echo has not landed.
      if (
        !force &&
        term?.buffer.active === buf &&
        anchor.line >= 0 &&
        cursorRow(buf) <= anchor.line
      ) {
        return;
      }
      settled = true;
      subscription?.dispose();
      if (timer !== null) clearTimeout(timer);
      if (!$settings.commandTime || !term || buf.type !== "normal") {
        anchor.dispose();
        return;
      }
      renderStamp(buf, anchor, at);
    };
    subscription = term.onWriteParsed(() => finalize(false));
    timer = setTimeout(() => finalize(true), 400);
  }

  /** Called from the key handler on a plain-Enter keydown, *before* xterm
   *  encodes it. Nothing here may change what is sent to the shell: no
   *  `preventDefault`, no `false` return, no write to the buffer. */
  function noteSubmit() {
    if (!term) return;
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
    // still a prompt, so the judgement holds. This read is only the *decision*
    // (is this a command?); the stamp's column is measured later, once the echo
    // has settled — see `stampCommand`.
    const row = buf.baseY + buf.cursorY;
    let first = row;
    while (first > 0 && buf.getLine(first)?.isWrapped) first--;
    let line = "";
    for (let r = first; r <= row; r++) {
      line += buf.getLine(r)?.translateToString(true) ?? "";
    }
    // The sidebar's history mode records what this page submitted, whatever
    // the stamp setting is — the history list is not a rendering option.
    const command = commandOnLine(line);
    if (command) dispatch("submit", command);
    if (!$settings.commandTime) return;
    if (!isCommandLine(line)) return;
    stampCommand(new Date());
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

    // Files copied in the OS file manager (Explorer / Nautilus / Finder: Ctrl+C
    // on a selection) arrive as a `paste` event whose `clipboardData.files`
    // carries them — there is no other browser API for that gesture. Without
    // this listener the event goes to the fork's own text-paste handler, which
    // finds an empty `text/plain` and silently drops the files.
    //
    // Capture phase on purpose: it runs before the textarea's listener, so
    // `stopPropagation()` keeps that handler out of a file-only paste (it must
    // still handle text pastes untouched).
    term.element?.addEventListener(
      "paste",
      (event: ClipboardEvent) => {
        const data = event.clipboardData;
        if (!data || data.files.length === 0) return;
        const payload = readDropPayload(data);
        if (payload.entries.length === 0 && payload.files.length === 0) return;
        event.preventDefault();
        event.stopPropagation();
        term?.focus();
        // Same path as a drop: the parent asks the shell for its pwd and
        // uploads there (targets, toasts and the transfer panel all apply).
        dispatch("dropfiles", { payload });
      },
      true,
    );

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
