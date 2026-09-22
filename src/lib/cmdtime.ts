// Command-submit timestamps (已知坑 99) — the text side of the feature: decide
// whether the line under the cursor is "shell prompt + command" and format the
// time that goes into the stamp after it.
//
// Pure string functions with no xterm dependency, on purpose: the judgement
// only needs the logical line's text, so the local and the remote terminal
// share one implementation. (The server side could have been authoritative for
// the local PTY — `tcgetpgrp(pty_master) == shell pid` means "the shell is at a
// prompt" — but a remote terminal has no local PTY at all, so it would have
// been a second, divergent code path plus a protocol message.)
//
// The heuristic is deliberately conservative: a false negative just means a
// command without a stamp, while a false positive would stamp time inside a
// program's own prompt. See `开发记录文档.md` 坑 99 for the full case table.

/** Characters that can end a shell prompt. `❯`/`➜` are the modern
 *  starship/pure/oh-my-zsh markers; `>` covers `bash -c`-style and
 *  `user@host~>` prompts, `%` is zsh's classic. */
const PROMPT_TAILS = "$#%❯➜>";

/** A prompt token at column 0 is only believed for the classic shell markers.
 *  `❯` and `>` at the very start of a line are what an agent CLI's inline input
 *  box (claude's `❯ `) and a REPL look like — those must not be stamped. */
const BARE_TAILS = "$#%➜";

/** Anything in here before the prompt token marks it as a *shell* prompt:
 *  `root@serve:/data# `, `user@h:~/proj$ `, `[root@h ~]# `, `➜ proj ❯ `,
 *  `PS C:\> `. Without it, `postgres=# `, `mysql> `, `>>> ` and `In [1]: `
 *  would all look like a shell. */
const PROMPT_PREFIX = /[@:/~\]\\]/;

/** Prompts are short; scanning further would start matching the command body
 *  (a `$ ` inside a shell script argument, say). */
const PROMPT_SCAN = 64;

/** Cells of blank kept between the command and the stamp, and between the stamp
 *  and the terminal's right edge: the stamp is right-aligned, so the same
 *  margin is kept on both of its sides. */
export const COMMAND_TIME_GAP = 2;

/** Column of the prompt's last character, or -1 when the line is not a prompt.
 *
 *  Only the **first** plausible token is judged: it is either the prompt or a
 *  command body that merely looks like one, and a real prompt never appears
 *  further right on its own line. */
export function promptEndColumn(line: string): number {
  const head = line.slice(0, PROMPT_SCAN);
  for (let i = 0; i < head.length; i++) {
    const ch = head[i];
    // A prompt token is followed by exactly one space (the shell's own trailing
    // space, or the first space the user typed after it).
    if (!PROMPT_TAILS.includes(ch) || head[i + 1] !== " ") continue;
    if (i === 0) return BARE_TAILS.includes(ch) ? 0 : -1;
    return PROMPT_PREFIX.test(head.slice(0, i)) ? i : -1;
  }
  return -1;
}

/** Is this a shell prompt with something typed after it? Pressing Enter on an
 *  empty prompt runs nothing, so it gets no stamp. */
export function isCommandLine(line: string): boolean {
  const end = promptEndColumn(line);
  return end >= 0 && line.slice(end + 1).trim() !== "";
}

const pad = (value: number) => String(value).padStart(2, "0");

/** `YYYY-MM-DD HH:mm:ss` (19 cells), local time. */
export function formatCommandTime(date: Date): string {
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}` +
    ` ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  );
}

/** `HH:mm:ss` (8 cells) — what a long command line falls back to. */
export function shortCommandTime(date: Date): string {
  return (
    `${pad(date.getHours())}:${pad(date.getMinutes())}:` +
    `${pad(date.getSeconds())}`
  );
}

/** The stamp text for a command ending at `endColumn` (in cells) on a terminal
 *  `cols` wide: the full date-time when it fits, the bare time when only that
 *  fits, and `null` when neither does — the stamp is never allowed to cover the
 *  user's own command.
 *
 *  The stamp is right-aligned, so it occupies the `text.length` cells ending
 *  `COMMAND_TIME_GAP` cells before the terminal's right edge; it fits when its
 *  left edge still clears the command by one `COMMAND_TIME_GAP`. */
export function commandTimeFor(
  date: Date,
  endColumn: number,
  cols: number,
): string | null {
  const fits = (length: number) =>
    endColumn + COMMAND_TIME_GAP + length + COMMAND_TIME_GAP <= cols;
  const full = formatCommandTime(date);
  if (fits(full.length)) return full;
  const short = shortCommandTime(date);
  if (fits(short.length)) return short;
  return null;
}
