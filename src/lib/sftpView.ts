import { SFTP_VIEW_STORAGE_KEY } from "$lib/constants";
import { storageGet, storageSet } from "$lib/storage";

/** File-manager view (browsed shell + path), persisted so a refresh returns to
 *  the same directory. */
export type SftpView = { path: string; viewShellId: number };

/** Read the persisted file-manager view, or null when absent/invalid. */
export function readSftpView(): SftpView | null {
  return storageGet<SftpView | null>(SFTP_VIEW_STORAGE_KEY, null, (raw) => {
    const parsed = JSON.parse(raw) as Partial<SftpView>;
    if (
      typeof parsed.path === "string" &&
      typeof parsed.viewShellId === "number"
    ) {
      return { path: parsed.path, viewShellId: parsed.viewShellId };
    }
    return null;
  });
}

/** Persist the file-manager view (called on every navigation). */
export function writeSftpView(path: string, viewShellId: number): void {
  storageSet(SFTP_VIEW_STORAGE_KEY, { path, viewShellId });
}
