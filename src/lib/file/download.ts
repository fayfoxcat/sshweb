/** @file Pure download helpers for the file manager (SFTP HTTP endpoints,
 *  native-download triggering, archive naming). Extracted from
 *  `FileManager.svelte` so the component keeps only its stateful glue. */

import { ARCHIVE_NAME_CAP } from "$lib/constants";
import { tr } from "$lib/i18n";

/** HTTP prefix for a session + shell's SFTP endpoints. */
export function sftpHttpPath(sessionName: string, shell: number): string {
  return `/api/s/${sessionName}/sftp/${shell}`;
}

/** Trigger a native browser download of `url` as `name`. */
export function triggerBrowserDownload(url: string, name: string): void {
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
}

/** Build the archive filename for a selection.
 *
 * - Single folder: `folder.zip`
 * - Multiple items: `name1、name2、name3等N个文件.zip`, capped at 40 chars.
 */
export function archiveNameFor(entries: { name: string }[]): string {
  if (entries.length === 1) {
    return `${entries[0].name}.zip`;
  }
  const MAX = ARCHIVE_NAME_CAP;
  const suffix = tr("file.zipSuffix", { n: entries.length });
  const sep = tr("file.zipSeparator");
  const names = entries.map((e) => e.name);
  const head = `${names.join(sep)}`;
  const full = `${head}${suffix}`;
  if (full.length <= MAX) return full;
  // Trim leading names to fit, always keeping at least the first name.
  let used = "";
  for (const n of names) {
    const candidate = used ? `${used}${sep}${n}` : n;
    if (`${candidate}${suffix}`.length <= MAX) {
      used = candidate;
    } else {
      break;
    }
  }
  if (!used) used = names[0];
  return `${used}${suffix}`;
}
