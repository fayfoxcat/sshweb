/**
 * @file Incremental per-line diff tracking for the editor.
 *
 * Instead of text-diffing the whole document (which is fragile: greedy
 * alignment mislabels identical lines like empty ones, and trailing-newline
 * differences turn unedited files blue), we track every CodeMirror change as
 * it happens. Each change tells us exactly which old lines were replaced by
 * which new lines, so flags are anchored to the real edit location.
 *
 * Statuses are keyed by 1-based line number of the *current* document:
 * - "added"    (green): the line was inserted and did not exist before.
 * - "modified" (blue):  the line existed but its content changed.
 */

export type DiffStatus = "added" | "modified";

/** Apply a document change to the previous per-line flags.
 *
 * @param startDoc   the document before the change
 * @param newDoc     the document after the change
 * @param changes    the changes between the two documents
 * @param prevFlags  per-line flags keyed by lines of `startDoc`
 * @returns flags keyed by lines of `newDoc`
 */
export function applyDocChanges(
  startDoc: any,
  newDoc: any,
  changes: any,
  prevFlags: Map<number, DiffStatus>,
): Map<number, DiffStatus> {
  // 1. Re-key existing flags from old-doc lines to new-doc lines.
  const flags = new Map<number, DiffStatus>();
  for (const [lineNo, status] of prevFlags) {
    if (lineNo < 1 || lineNo > startDoc.lines) continue;
    const pos = startDoc.line(lineNo).from;
    const newPos = changes.mapPos(pos, 1);
    if (newPos < 0 || newPos > newDoc.length) continue;
    const newLine = newDoc.lineAt(Math.min(newPos, newDoc.length)).number;
    flags.set(newLine, status);
  }

  // 2. Apply each change. Processing in reverse keeps new-document positions
  //    of earlier changes stable (later changes don't affect earlier spans).
  const edits: {
    fromA: number;
    toA: number;
    fromB: number;
    toB: number;
    ins: string;
  }[] = [];
  changes.iterChanges(
    (fromA: number, toA: number, fromB: number, toB: number, inserted: any) => {
      edits.push({ fromA, toA, fromB, toB, ins: inserted.toString() });
    },
  );

  for (const e of edits.reverse()) {
    const delLen = e.toA - e.fromA;
    const oldStartLine = startDoc.lineAt(e.fromA).number;
    // Inclusive end line: for a pure insertion at a line boundary the line
    // after the insertion is also affected (it shifts down), so use `toA`
    // rather than `toA - 1`.
    const oldEndPos = e.toA > e.fromA ? e.toA : e.fromA;
    const oldEndLine = startDoc.lineAt(oldEndPos).number;
    const newStartLine = newDoc.lineAt(e.fromB).number;
    const newEndPos = e.toB > e.fromB ? e.toB : e.fromB;
    const newEndLine = newDoc.lineAt(newEndPos).number;

    // Inline edit (no newline created/removed) -> the single line is modified.
    if (delLen === 0 && !e.ins.includes("\n")) {
      flags.set(newStartLine, "modified");
      continue;
    }

    // Structural change: compare the affected old/new lines, anchored by the
    // edit position so identical lines aren't misaligned.
    const oldLines: string[] = [];
    for (let i = oldStartLine; i <= oldEndLine; i++) {
      oldLines.push(startDoc.line(i).text);
    }
    const newLines: string[] = [];
    for (let i = newStartLine; i <= newEndLine; i++) {
      newLines.push(newDoc.line(i).text);
    }

    // If the edit starts at the beginning of a line, the old content shifts
    // down (anchor a common suffix); otherwise the old content stays at the
    // front (anchor a common prefix). This resolves the ambiguous empty-line
    // case ("press Enter next to a blank line").
    const atLineStart = e.fromA === startDoc.line(oldStartLine).from;
    let pre = 0;
    let suf = 0;
    if (atLineStart) {
      while (
        suf < oldLines.length &&
        suf < newLines.length &&
        oldLines[oldLines.length - 1 - suf] ===
          newLines[newLines.length - 1 - suf]
      ) {
        suf++;
      }
    } else {
      while (
        pre < oldLines.length &&
        pre < newLines.length &&
        oldLines[pre] === newLines[pre]
      ) {
        pre++;
      }
    }

    const midOld = oldLines.length - pre - suf;
    const midNew = newLines.length - pre - suf;
    if (midNew === 0) {
      // Pure deletion with nothing left in the region; mark where it collapsed.
      flags.set(newStartLine, "modified");
      continue;
    }

    const replaced = Math.min(midOld, midNew);
    for (let i = 0; i < midNew; i++) {
      const lineNo = newStartLine + pre + i;
      flags.set(lineNo, i < replaced ? "modified" : "added");
    }
  }

  return flags;
}
