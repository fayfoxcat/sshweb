import { get, writable } from "svelte/store";

import {
  UPLOAD_ACK_TIMEOUT_MS,
  UPLOAD_CHUNK,
  UPLOAD_MAX_RETRIES,
  UPLOAD_TASKS_KEY,
} from "./constants";
import { tr } from "./i18n";
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

/** In-flight upload keyed by `<shell>:<path>` — the same destination path on
 *  two different shells (servers) is two distinct uploads, so switching the
 *  file manager from server A to server B must never orphan A's upload (see
 *  已知坑: a Map keyed by path alone made the second upload overwrite the
 *  first, silently stopping it). Only one chunk per file is sent at a time;
 *  the next chunk is released by the previous ack, so a permission error on
 *  the first chunk never floods the server with the rest. */
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
  /** Number of distinct chunks (each acked offset counts once). */
  expected: number;
  /** Number of distinct offsets acknowledged by the server. */
  done: number;
  /** Offset of the NEXT chunk to send. Advances only on ack, so a retry
   *  re-reads exactly the unacknowledged bytes. */
  nextOffset: number;
  /** Offset of the chunk currently in flight, or null when none. */
  inFlightOffset: number | null;
  /** Byte length of the in-flight chunk. */
  inFlightLength: number;
  /** Offsets already acknowledged by the server. A retried chunk can be
   *  written twice (the original write happened but its ack was dropped, or a
   *  delayed ack surfaces after the retry), so acks are deduplicated by
   *  offset rather than counted blindly. */
  ackedOffsets: Set<number>;
  started: boolean;
  /** Watchdog timer for the in-flight chunk (cleared on ack). */
  watchdog: ReturnType<typeof setTimeout> | null;
  /** Consecutive watchdog timeouts for the current chunk. */
  retries: number;
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

/** Drop an in-flight upload from the map and mark its task failed. */
function failTask(upload: Upload, error: string) {
  uploads.delete(uploadKey(upload.targetShell, upload.targetPath));
  clearWatchdog(upload);
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
    // Delete the partial file on the server so nothing is left behind.
    running.targetSocket.send({
      sftpRemove: [running.targetShell, running.targetPath, false],
    });
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

function armWatchdog(upload: Upload) {
  clearWatchdog(upload);
  upload.watchdog = setTimeout(
    () => onChunkTimeout(upload),
    UPLOAD_ACK_TIMEOUT_MS,
  );
}

function clearWatchdog(upload: Upload) {
  if (upload.watchdog !== null) {
    clearTimeout(upload.watchdog);
    upload.watchdog = null;
  }
}

/** A chunk was sent but no ack arrived within the watchdog window. A dropped
 *  `sftpWriteOk` (the server's 512-bounded output queue is best-effort) would
 *  otherwise stall the upload forever. Retry the SAME offset: the write is
 *  idempotent, and the offset-echo ack deduplicates any duplicate acks that
 *  surface later (the original ack may have merely been delayed). */
function onChunkTimeout(upload: Upload) {
  if (uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload)
    return;
  if (upload.inFlightOffset === null) return;
  upload.retries += 1;
  if (upload.retries > UPLOAD_MAX_RETRIES) {
    failTask(upload, tr("file.taskTimeout"));
    return;
  }
  const chunkOffset = upload.inFlightOffset;
  const end = Math.min(chunkOffset + UPLOAD_CHUNK, upload.total);
  const reader = new FileReader();
  reader.onload = () => {
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    const data = new Uint8Array(reader.result as ArrayBuffer);
    if (data.length === 0 && chunkOffset < upload.total) {
      failTask(upload, tr("file.readFail"));
      return;
    }
    upload.inFlightLength = data.length;
    upload.targetSocket.send({
      sftpWriteAt: [upload.targetShell, upload.targetPath, chunkOffset, data],
    });
    armWatchdog(upload);
  };
  reader.onerror = () => {
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    failTask(upload, tr("file.readFail"));
  };
  reader.readAsArrayBuffer(upload.file.slice(chunkOffset, end));
}

/** Send exactly one chunk. The next chunk is sent only after the server
 *  acknowledges this one, so progress reflects bytes actually written. */
function sendNextChunk(upload: Upload) {
  if (uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload)
    return;
  if (upload.inFlightOffset !== null) return; // a chunk is in flight
  // Empty files still need one offset-0 write so the server creates them.
  if (upload.started && upload.nextOffset >= upload.total) return;

  const chunkOffset = upload.nextOffset;
  const end = Math.min(chunkOffset + UPLOAD_CHUNK, upload.total);
  const reader = new FileReader();
  upload.started = true;
  reader.onload = () => {
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    const data = new Uint8Array(reader.result as ArrayBuffer);
    if (data.length === 0 && chunkOffset < upload.total) {
      failTask(upload, tr("file.readFail"));
      return;
    }
    upload.inFlightOffset = chunkOffset;
    upload.inFlightLength = data.length;
    upload.retries = 0;
    upload.targetSocket.send({
      sftpWriteAt: [upload.targetShell, upload.targetPath, chunkOffset, data],
    });
    armWatchdog(upload);
  };
  reader.onerror = () => {
    if (
      uploads.get(uploadKey(upload.targetShell, upload.targetPath)) !== upload
    )
      return;
    failTask(upload, tr("file.readFail"));
  };
  reader.readAsArrayBuffer(upload.file.slice(chunkOffset, end));
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
  // attempt; orphan it so its task doesn't hang in "running".
  const prev = uploads.get(key);
  if (prev) {
    failTask(prev, tr("file.taskReplaced"));
  }
  // Show the target identity (user@host) in the transfer panel so it's
  // clear which user the file is uploaded as.
  const task = addTask({
    name: targetName ? `${targetName} · ${displayName}` : displayName,
    total: file.size,
  });
  const expected = Math.max(1, Math.ceil(file.size / UPLOAD_CHUNK));
  const uploadState: Upload = {
    taskId: task.id,
    file,
    displayName,
    targetSocket: socket,
    targetShell,
    targetPath: destPath,
    targetDir: destPath.slice(0, destPath.lastIndexOf("/")) || "/",
    total: file.size,
    expected,
    done: 0,
    nextOffset: 0,
    inFlightOffset: null,
    inFlightLength: 0,
    ackedOffsets: new Set(),
    started: false,
    watchdog: null,
    retries: 0,
    onDone,
  };
  uploads.set(key, uploadState);
  sendNextChunk(uploadState);
}

/** Handle an upload acknowledgement. The server echoes the written byte
 *  offset (`sftpWriteOk`); when absent (the old bare `sftpOk` protocol) the
 *  in-flight chunk's offset is assumed. Returns true when it advanced an
 *  upload (chunk acked); callers then do not run their own listing refresh. */
export function onUploadAck(
  savedShell: number,
  savedPath: string,
  offset?: number,
): boolean {
  const up = uploads.get(uploadKey(savedShell, savedPath));
  if (!up) return false;
  // Which offset this ack refers to: the echo when available, otherwise the
  // current in-flight chunk (backward-compatible with the old `sftpOk`).
  const ackedOffset = offset ?? up.inFlightOffset;
  if (ackedOffset === null) return false;
  // Deduplicate: a retried chunk can produce two acks for the same offset.
  if (up.ackedOffsets.has(ackedOffset)) return true;
  up.ackedOffsets.add(ackedOffset);
  clearWatchdog(up);
  up.done += 1;
  up.nextOffset = ackedOffset + up.inFlightLength;
  up.inFlightOffset = null;
  up.inFlightLength = 0;
  updateTask(up.taskId, { done: up.nextOffset });
  if (up.done >= up.expected) {
    uploads.delete(uploadKey(savedShell, savedPath));
    updateTask(up.taskId, { status: "done" });
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
  } else {
    sendNextChunk(up);
  }
  return true;
}

/** Handle an error message (`写入失败（<path>）：…` marks a failed chunk). */
export function onUploadError(message: string): void {
  const m = message.match(/写入失败（(.+?)）：/);
  if (!m) return;
  const path = m[1];
  // The server error carries only the path; a path shared by uploads on
  // several shells is ambiguous — fail every matching upload (safer than a
  // silent stall) so the user sees the error and can retry.
  let matched = false;
  for (const [key, up] of uploads) {
    if (up.targetPath !== path) continue;
    uploads.delete(key);
    clearWatchdog(up);
    updateTask(up.taskId, { status: "error", error: message });
    matched = true;
  }
  if (matched) persistTasks();
}
