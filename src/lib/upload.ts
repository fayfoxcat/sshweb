import { get, writable } from "svelte/store";

import {
  UPLOAD_ACK_TIMEOUT_MS,
  UPLOAD_CHUNK,
  UPLOAD_MAX_INFLIGHT,
  UPLOAD_MAX_RETRIES,
  UPLOAD_TASKS_KEY,
} from "./constants";
import { tr } from "./i18n";
import { parentOf } from "./path";
import type { WsClient, WsServer } from "./protocol";
import type { Srocket } from "./srocket";
import { storageGet, storageSet } from "./storage";
import { makeToast } from "./toast";

/** One entry in the bottom-right transfer panel (upload only). */
type UploadTask = {
  id: number;
  name: string;
  total: number;
  done: number;
  status: "running" | "done" | "error";
  error?: string;
};

/** One chunk handed to the socket and not yet acknowledged. */
type InflightChunk = {
  /** Byte offset of the chunk's first byte (also its ack identity). */
  offset: number;
  /** Bytes actually read from the File for this chunk (0 until the read ends). */
  length: number;
  /** True once the chunk's bytes have been handed to the socket. Distinguishes
   *  "still being read" from "sent, but zero bytes long" — an *empty* file's
   *  single chunk is legitimately 0 bytes, and it still has to be resent when
   *  its ack is lost (otherwise the upload would sit there forever). */
  sent: boolean;
  /** When the chunk was (last) sent, for the watchdog. */
  sentAt: number;
  /** How many times this chunk's watchdog fired (resends). */
  attempts: number;
};

/** In-flight upload keyed by `<shell>:<path>` — the same destination path on
 *  two different shells (servers) is two distinct uploads, so switching the
 *  file manager from server A to server B must never orphan A's upload (see
 *  已知坑: a Map keyed by path alone made the second upload overwrite the
 *  first, silently stopping it).
 *
 *  Up to `UPLOAD_MAX_INFLIGHT` chunks are in flight at once. This is safe
 *  *because* the server applies the chunks through one FIFO write worker:
 *  arrival order is send order, so `offset 0`'s truncate always runs before
 *  any later chunk of the same file — the ordering guarantee the old
 *  one-chunk-at-a-time pacing was silently providing (已知坑 19/77). The ack
 *  offset is still the only thing that advances progress, so a lost ack is
 *  retried (idempotent write) rather than double-counted. */
type Upload = {
  taskId: number;
  file: File;
  displayName: string;
  targetSocket: Srocket<WsServer, WsClient>;
  targetShell: number;
  targetPath: string;
  /** Directory the file was uploaded into (the parent of `targetPath`), shown
   *  in the completion toast so the user sees exactly where it went. */
  targetDir: string;
  /** Total bytes to upload (the File's size at start). */
  total: number;
  /** Bytes acknowledged so far (sum of acked chunk lengths). */
  ackedBytes: number;
  /** Index of the next chunk to start reading. */
  nextChunk: number;
  /** True while a `FileReader` is filling the next chunk. Only one read runs
   *  at a time so chunks leave strictly in ascending offset order (see
   *  `pump`). */
  reading: boolean;
  /** Sent, unacknowledged chunks, in send order. */
  inflight: InflightChunk[];
  /** Chunk indexes already acknowledged (acks are deduplicated by chunk, so a
   *  retried chunk whose original ack surfaces later is not counted twice). */
  acked: Set<number>;
  /** True once a chunk has been handed to the socket. Every chunk may have
   *  been written even if its ack never arrived, so this — not `ackedBytes` —
   *  is what decides whether the server could be holding a partial file that a
   *  failure must clean up. */
  wroteAny: boolean;
  /** True while the final `sftpUploadDone` awaits the server's size check
   *  (`sftpUploadOk`). No chunk is in flight then, so the watchdog has to
   *  retry the completion message instead of a chunk. */
  verifying: boolean;
  /** When the completion message was (last) sent, for the watchdog. */
  verifyingSince: number;
  /** Consecutive watchdog timeouts of the completion message. */
  verifyAttempts: number;
  started: boolean;
  /** Watchdog timer, armed for the oldest outstanding deadline. */
  watchdog: ReturnType<typeof setTimeout> | null;
  onDone: () => void;
};

/** One file/folder from a drag-drop upload, with its path relative to the
 *  drop root (preserves the folder sub-tree). */
export type DropFile = { file: File; relPath: string };

/** The synchronous snapshot of a `drop` event's `DataTransfer`: the top-level
 *  `FileSystemEntry`s (from `webkitGetAsEntry`, the only way to get folder
 *  structure on drag-drop — `dataTransfer.files` has no `webkitRelativePath`
 *  for drops) plus the raw files as a fallback. Must be captured inside the
 *  drop handler; the entries stay usable afterwards. */
export type DropPayload = {
  entries: FileSystemEntry[];
  files: File[];
};

/** Snapshot a drop's DataTransfer synchronously (inside the drop handler). */
export function readDropPayload(dt: DataTransfer): DropPayload {
  const entries = [...dt.items]
    .filter((item) => item.kind === "file")
    .map((item) => item.webkitGetAsEntry?.())
    .filter((e): e is FileSystemEntry => Boolean(e));
  return { entries, files: [...dt.files] };
}

/** Recursively walk a drop payload, yielding one entry per file with its
 *  relative path under the drop root. Directories are descended via
 *  `createReader` (each read returns up to ~100 entries, so loop until empty).
 *  Falls back to the plain `files` list when `webkitGetAsEntry` is
 *  unavailable. */
export async function collectDropFiles(
  payload: DropPayload,
): Promise<DropFile[]> {
  if (payload.entries.length === 0) {
    return payload.files.map((file) => ({ file, relPath: file.name }));
  }
  const out: DropFile[] = [];
  async function readAll(
    reader: FileSystemDirectoryReader,
  ): Promise<FileSystemEntry[]> {
    const all: FileSystemEntry[] = [];
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((resolve) => {
        reader.readEntries(resolve, () => resolve([]));
      });
      if (batch.length === 0) break;
      all.push(...batch);
    }
    return all;
  }
  async function walk(entry: FileSystemEntry, rel: string): Promise<void> {
    if (entry.isDirectory) {
      const dirRel = rel ? `${rel}/${entry.name}` : entry.name;
      const reader = (entry as FileSystemDirectoryEntry).createReader();
      for (const child of await readAll(reader)) {
        await walk(child, dirRel);
      }
    } else {
      const file = await new Promise<File>((resolve, reject) => {
        (entry as FileSystemFileEntry).file(resolve, () =>
          reject(new Error("read failed")),
        );
      });
      out.push({ file, relPath: rel ? `${rel}/${file.name}` : file.name });
    }
  }
  for (const entry of payload.entries) {
    await walk(entry, "");
  }
  return out;
}

// Exactly one FileManager instance mounts per session, so module-level state
// (instead of component state) is safe here.
const uploads = new Map<string, Upload>();
export const uploadTasks = writable<UploadTask[]>([]);
let nextTaskId = 1;

/** Composite key binding an upload to its destination shell AND path, so
 *  parallel uploads to the same path on different servers never collide. */
function uploadKey(shell: number, path: string): string {
  return `${shell}:${path}`;
}

/** Persist the task list (sessionStorage survives a browser refresh) so the
 *  transfer panel button and finished/interrupted records stay visible until
 *  the user clears them. */
function persistTasks() {
  storageSet(UPLOAD_TASKS_KEY, get(uploadTasks));
}

/** Restore tasks persisted before the page unloaded. Running tasks cannot be
 *  resumed after a refresh — the browser cannot re-read the File object that
 *  was in memory — so they are marked interrupted instead of showing a
 *  zombie "running" state forever. */
function restoreTasks() {
  const saved = storageGet<UploadTask[]>(UPLOAD_TASKS_KEY, [], (raw) => {
    const parsed = JSON.parse(raw) as UploadTask[];
    return Array.isArray(parsed) ? parsed : [];
  });
  if (saved.length === 0) return;
  const restored = saved.map((t) =>
    t.status === "running"
      ? { ...t, status: "error" as const, error: tr("file.taskInterrupted") }
      : t,
  );
  for (const t of restored) {
    nextTaskId = Math.max(nextTaskId, t.id + 1);
  }
  uploadTasks.set(restored);
}
restoreTasks();

function addTask(init: Omit<UploadTask, "id" | "done" | "status">): UploadTask {
  const task: UploadTask = {
    ...init,
    id: nextTaskId++,
    done: 0,
    status: "running",
  };
  uploadTasks.update((tasks) => [...tasks, task]);
  persistTasks();
  return task;
}

function updateTask(id: number, patch: Partial<UploadTask>) {
  uploadTasks.update((tasks) =>
    tasks.map((t) => (t.id === id ? { ...t, ...patch } : t)),
  );
  persistTasks();
}

/** Ask the server to delete a partial upload, but only once at least one chunk
 *  was handed to the socket. Before that the path may hold the user's
 *  pre-existing file, which no chunk has touched — deleting it would be a
 *  destructive surprise. (The abort travels the same ordered WebSocket as the
 *  chunks, so if it arrives, the chunks did too.) */
function sendAbort(upload: Upload) {
  if (!upload.wroteAny) return;
  upload.targetSocket.send({
    sftpUploadAbort: [upload.targetShell, upload.targetPath],
  });
}

/** Drop an in-flight upload from the map and mark its task failed. When the
 *  upload had already sent bytes, the server is asked to delete the partial
 *  file (`sftpUploadAbort`, queued behind every chunk sent so far): a
 *  half-written file left under the final name looks like a good file until
 *  something reads it — `pg_restore` reports exactly that as
 *  "could not read from input file: end of file". */
function failTask(upload: Upload, error: string, abort = true) {
  uploads.delete(uploadKey(upload.targetShell, upload.targetPath));
  clearWatchdog(upload);
  if (abort) sendAbort(upload);
  updateTask(upload.taskId, { status: "error", error });
}

/** Cancel an in-flight upload: stop sending further chunks and ask the server
 *  to remove the partially-written file (no leftovers). Only applies while the
 *  task is still running; a finished/failed record is just removed. */
export function cancelUploadTask(id: number) {
  // Find the running upload for this task.
  const running = [...uploads.values()].find((up) => up.taskId === id);
  if (running) {
    // Drop it from the in-flight map so no further chunk is sent (the
    // FileReader's onload guards against this too).
    uploads.delete(uploadKey(running.targetShell, running.targetPath));
    clearWatchdog(running);
    // Delete the partial file on the server. The abort shares the server's
    // per-session write FIFO with the chunks, so it cannot be overtaken by a
    // chunk that is still in flight (which would recreate the file).
    sendAbort(running);
  }
  uploadTasks.update((tasks) => tasks.filter((t) => t.id !== id));
  persistTasks();
}

/** Clear finished/error task records from the transfer panel, keeping any
 *  still-running uploads. */
export function clearUploadTasks() {
  uploadTasks.update((tasks) => tasks.filter((t) => t.status === "running"));
  persistTasks();
}

/** Re-arm the upload's watchdog for the **oldest** outstanding deadline.
 *
 *  One timer per upload (not per chunk): with several chunks in flight the
 *  earliest deadline is the only one that matters, and re-arming after every
 *  event keeps a single code path for both "waiting for a chunk ack" and
 *  "waiting for the completion check". */
function armWatchdog(upload: Upload) {
  clearWatchdog(upload);
  const now = Date.now();
  const deadline = upload.verifying
    ? upload.verifyingSince + UPLOAD_ACK_TIMEOUT_MS
    : upload.inflight.length > 0
      ? Math.min(...upload.inflight.map((c) => c.sentAt)) +
        UPLOAD_ACK_TIMEOUT_MS
      : null;
  if (deadline === null) return;
  upload.watchdog = setTimeout(
    () => onTimeout(upload),
    Math.max(deadline - now, 0),
  );
}

function clearWatchdog(upload: Upload) {
  if (upload.watchdog !== null) {
    clearTimeout(upload.watchdog);
    upload.watchdog = null;
  }
}

/** Hand one already-read chunk to the socket and start its watchdog window. */
function transmitChunk(
  upload: Upload,
  chunk: InflightChunk,
  data: Uint8Array,
): void {
  chunk.length = data.length;
  chunk.sent = true;
  chunk.sentAt = Date.now();
  // From here on the server may hold part of this file, so a later failure
  // must clean it up (see `failTask`).
  upload.wroteAny = true;
  upload.targetSocket.send({
    sftpWriteAt: [upload.targetShell, upload.targetPath, chunk.offset, data],
  });
  armWatchdog(upload);
}

/** Read one chunk of the file and send it. `chunk` is already registered in
 *  `upload.inflight`, so a watchdog firing while the read is running resends
 *  through the same entry instead of sending a second copy. */
function readAndSend(upload: Upload, chunk: InflightChunk) {
  const end = Math.min(chunk.offset + UPLOAD_CHUNK, upload.total);
  const reader = new FileReader();
  reader.onload = () => {
    // The read finished: the read slot is free whatever happens next (the
    // upload may even have been superseded meanwhile).
    upload.reading = false;
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    const data = new Uint8Array(reader.result as ArrayBuffer);
    if (data.length === 0 && chunk.offset < upload.total) {
      failTask(upload, tr("file.readFail"));
      return;
    }
    transmitChunk(upload, chunk, data);
    pump(upload);
  };
  reader.onerror = () => {
    upload.reading = false;
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    failTask(upload, tr("file.readFail"));
  };
  reader.readAsArrayBuffer(upload.file.slice(chunk.offset, end));
}

/** Number of chunks making up the file (at least one: an empty file still
 *  needs its offset-0 write so the server creates it). */
function chunkCount(upload: Upload): number {
  return Math.max(1, Math.ceil(upload.total / UPLOAD_CHUNK));
}

/** Keep the pipeline full, and finish the upload once every byte is acked.
 *
 *  **Chunks are read in strictly ascending offset order** — only one
 *  `FileReader` runs at a time — even though up to `UPLOAD_MAX_INFLIGHT`
 *  chunks may be outstanding on the wire. That ordering is load-bearing, not
 *  tidiness: the server's `write_at_remote` truncates the file when it sees
 *  `offset == 0`, and the pieces of every *other* chunk open without
 *  `truncate`, so a 0-offset chunk that arrives late would wipe whatever was
 *  already written (已知坑 77, exactly the failure a first cut of this change
 *  produced: `FileReader` callbacks may finish out of order, so pipelining the
 *  *reads* silently reordered the sends and the uploaded file was corrupt with
 *  the right length). Reads are local disk I/O — serializing them costs
 *  nothing measurable, while the network keeps three chunks in flight.
 */
function pump(upload: Upload) {
  if (uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload)
    return;
  if (upload.verifying) return; // awaiting the completion check

  const total = chunkCount(upload);
  if (
    !upload.reading &&
    upload.nextChunk < total &&
    upload.inflight.length < UPLOAD_MAX_INFLIGHT
  ) {
    const offset = upload.nextChunk * UPLOAD_CHUNK;
    upload.nextChunk += 1;
    upload.reading = true;
    upload.started = true;
    const chunk: InflightChunk = {
      offset,
      length: 0,
      sent: false,
      sentAt: Date.now(),
      attempts: 0,
    };
    upload.inflight.push(chunk);
    readAndSend(upload, chunk);
  }

  // Completion is decided by BYTES, never by the number of acks: a chunk that
  // came back shorter than `UPLOAD_CHUNK` would otherwise let the ack count
  // reach the expected number while the file is still short.
  if (
    upload.ackedBytes >= upload.total &&
    upload.inflight.length === 0 &&
    !upload.reading
  ) {
    sendUploadDone(upload);
    return;
  }
  armWatchdog(upload);
}

/** A chunk (or the completion check) outlived the watchdog window. A dropped
 *  `sftpWriteOk` (the server's 512-bounded output queue is best-effort) would
 *  otherwise stall the upload forever. Retry the SAME offsets: the writes are
 *  idempotent, and the offset-echo ack deduplicates acks that surface late. */
function onTimeout(upload: Upload) {
  if (uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload)
    return;
  if (upload.verifying) {
    // The completion message (or its size-check reply) was lost — resend it.
    // Re-verifying an already-verified upload is harmless: the size still
    // matches, so the server answers `sftpUploadOk` again.
    upload.verifyAttempts += 1;
    if (upload.verifyAttempts > UPLOAD_MAX_RETRIES) {
      failTask(upload, tr("file.taskTimeout"));
      return;
    }
    sendUploadDone(upload);
    return;
  }

  const now = Date.now();
  const stale = upload.inflight.filter(
    (chunk) => now - chunk.sentAt >= UPLOAD_ACK_TIMEOUT_MS,
  );
  for (const chunk of stale) {
    chunk.attempts += 1;
    if (chunk.attempts > UPLOAD_MAX_RETRIES) {
      failTask(upload, tr("file.taskTimeout"));
      return;
    }
    // A chunk still being read (its `FileReader` has not fired yet) is just
    // re-stamped: the read in progress will send it once. `sent` — not
    // `length > 0` — is the test, because an empty file's only chunk is a
    // legitimately 0-byte write that must still be retried.
    chunk.sentAt = now;
    if (chunk.sent) {
      // Already sent once: re-read and re-send the same range. This is a
      // *resend of an existing chunk*, so it must not take the read slot
      // (`upload.reading`) — `pump` only ever reads the next, not-yet-sent
      // offset, and taking the slot would stall that pipeline for a whole
      // round trip.
      const resend = chunk;
      const end = Math.min(resend.offset + UPLOAD_CHUNK, upload.total);
      const reader = new FileReader();
      reader.onload = () => {
        if (
          uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !==
          upload
        )
          return;
        const data = new Uint8Array(reader.result as ArrayBuffer);
        if (data.length === 0 && resend.offset < upload.total) {
          failTask(upload, tr("file.readFail"));
          return;
        }
        transmitChunk(upload, resend, data);
      };
      reader.onerror = () => {
        if (
          uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !==
          upload
        )
          return;
        failTask(upload, tr("file.readFail"));
      };
      reader.readAsArrayBuffer(upload.file.slice(resend.offset, end));
    }
  }
  armWatchdog(upload);
}

/** Send the completion message for an upload whose last byte was acked and
 *  wait for the server's size check. Acknowledged chunks only prove that each
 *  chunk's write returned — not that the file on disk holds every byte — so
 *  the upload is finished by the server's verification, never by the ack
 *  count. */
function sendUploadDone(upload: Upload) {
  upload.verifying = true;
  upload.verifyingSince = Date.now();
  upload.targetSocket.send({
    sftpUploadDone: [upload.targetShell, upload.targetPath, upload.total],
  });
  armWatchdog(upload);
}

/** Upload a file in acknowledged chunks. `destPath` overrides the default
 *  current-directory target (used for folder uploads to preserve the
 *  sub-directory structure). `onDone` runs once the final chunk is acked. */
export function startUpload(opts: {
  file: File;
  destPath: string;
  displayName: string;
  targetShell: number;
  targetName: string;
  socket: Srocket<WsServer, WsClient>;
  onDone: () => void;
}): void {
  const {
    file,
    destPath,
    displayName,
    targetShell,
    targetName,
    socket,
    onDone,
  } = opts;
  const key = uploadKey(targetShell, destPath);
  // Re-uploading the same path on the same shell supersedes the previous
  // attempt; orphan it so its task doesn't hang in "running". No abort is
  // sent: this upload starts with a truncating offset-0 chunk, so the old
  // partial file is overwritten — and an abort queued behind that chunk would
  // delete the *new* file instead.
  const prev = uploads.get(key);
  if (prev) {
    failTask(prev, tr("file.taskReplaced"), false);
  }
  // Show the target identity (user@host) in the transfer panel so it's
  // clear which user the file is uploaded as.
  const task = addTask({
    name: targetName ? `${targetName} · ${displayName}` : displayName,
    total: file.size,
  });
  const uploadState: Upload = {
    taskId: task.id,
    file,
    displayName,
    targetSocket: socket,
    targetShell,
    targetPath: destPath,
    targetDir: parentOf(destPath),
    total: file.size,
    ackedBytes: 0,
    nextChunk: 0,
    reading: false,
    inflight: [],
    acked: new Set(),
    wroteAny: false,
    verifying: false,
    verifyingSince: 0,
    verifyAttempts: 0,
    started: false,
    watchdog: null,
    onDone,
  };
  uploads.set(key, uploadState);
  pump(uploadState);
}

/** Handle an upload acknowledgement. The server echoes the written byte
 *  offset (`sftpWriteOk`); when absent (a bare `sftpOk`) the oldest in-flight
 *  chunk is assumed. Returns true when it belonged to a tracked upload (chunk
 *  acked); callers then do not run their own listing refresh. */
export function onUploadAck(
  savedShell: number,
  savedPath: string,
  offset?: number,
): boolean {
  const up = uploads.get(uploadKey(savedShell, savedPath));
  if (!up) return false;
  // An ack for a finished upload (or one interleaved with the completion
  // check) is not a chunk ack: there is no chunk in flight to associate it
  // with, and the offset echo is what keeps a bare `sftpOk` honest.
  if (up.verifying) return false;
  const ackedOffset = offset ?? up.inflight[0]?.offset;
  if (ackedOffset === undefined) return false;
  // Find the chunk this ack belongs to. With several chunks in flight the
  // offset echo is what identifies it — counting acks would mis-attribute a
  // retried chunk's late ack to the wrong position.
  const at = up.inflight.findIndex((chunk) => chunk.offset === ackedOffset);
  if (at === -1) {
    // Already acked (a duplicate ack for a retried chunk): report it as
    // handled so nothing regresses, but do not advance progress twice.
    return up.acked.has(ackedOffset / UPLOAD_CHUNK);
  }
  const [chunk] = up.inflight.splice(at, 1);
  up.acked.add(ackedOffset / UPLOAD_CHUNK);
  up.ackedBytes += chunk.length;
  updateTask(up.taskId, { done: up.ackedBytes });
  pump(up);
  return true;
}

/** Handle the server's upload verification (`sftpUploadOk`): the file on disk
 *  is complete — its size equals what was uploaded. This — not the last chunk
 *  ack — is what marks an upload successful. Returns true when it belongs to a
 *  tracked upload (callers then skip their own listing refresh). */
export function onUploadVerified(
  savedShell: number,
  savedPath: string,
  size: number,
): boolean {
  const up = uploads.get(uploadKey(savedShell, savedPath));
  if (!up) return false;
  if (size !== up.total) {
    // The server only acks a verified size, so this means the two sides
    // disagree about what was uploaded — trust neither and fail loudly.
    failTask(up, tr("file.taskVerify"));
    return true;
  }
  uploads.delete(uploadKey(savedShell, savedPath));
  clearWatchdog(up);
  updateTask(up.taskId, { status: "done", done: up.total });
  // Report each completed upload once, by its display name and the target
  // directory — so a terminal drop that went to the shell's pwd (not the
  // file manager's folder) is immediately visible.
  makeToast({
    kind: "success",
    message: tr("file.uploaded", {
      name: up.displayName,
      dir: up.targetDir,
    }),
  });
  up.onDone(); // the new file should now appear in the listing
  return true;
}

/** Handle an error message (`写入失败（<path>）：…` marks a failed chunk, a
 *  failed verification, or a rejected save). */
export function onUploadError(message: string): void {
  const m = message.match(/写入失败（(.+?)）：/);
  if (!m) return;
  const path = m[1];
  // The server error carries only the path; a path shared by uploads on
  // several shells is ambiguous — fail every matching upload (safer than a
  // silent stall) so the user sees the error and can retry. Failing also
  // deletes the partial file the failed write left behind.
  let matched = false;
  for (const up of [...uploads.values()]) {
    if (up.targetPath !== path) continue;
    failTask(up, message);
    matched = true;
  }
  if (matched) persistTasks();
}
