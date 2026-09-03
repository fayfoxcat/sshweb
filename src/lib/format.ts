/** Pick the binary-prefix scale for a byte count: the divisor and prefix letter
 *  ("", "K", "M", "G"). `giga` enables the GB scale (file sizes and rates both
 *  use it; the rate formatter also needs it for GB/s). */
function byteScale(
  bytes: number,
  giga = false,
): { div: number; prefix: "G" | "M" | "K" | "" } {
  if (giga && bytes >= 1024 ** 3) return { div: 1024 ** 3, prefix: "G" };
  if (bytes >= 1024 ** 2) return { div: 1024 ** 2, prefix: "M" };
  if (bytes >= 1024) return { div: 1024, prefix: "K" };
  return { div: 1, prefix: "" };
}

/** Format a file size in a human-readable way: B / KB / MB / GB, two decimals
 *  for the scaled units (bytes stay whole). */
export function formatSize(size: number): string {
  const { div, prefix } = byteScale(size, true);
  if (prefix === "") return `${size} B`;
  return `${(size / div).toFixed(2)} ${prefix}B`;
}

/** Format unix seconds as `YYYY-MM-DD HH:mm:ss` (or "未知" when absent). */
import { tr } from "./i18n";

export function formatDateTime(secs?: number): string {
  if (!secs) return tr("common.unknown");
  const d = new Date(secs * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(
    d.getDate(),
  )} ${clockTime(d)}`;
}

/** Format unix seconds as `YYYY-MM-DD`. */
export function formatDate(secs: number): string {
  const d = new Date(secs * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** Format unix seconds as `HH:mm:ss`. */
export function formatClock(secs: number): string {
  return clockTime(new Date(secs * 1000));
}

function clockTime(d: Date): string {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** Format Unix mode bits as a 10-character `drwxr-xr-x` string. */
export function formatMode(mode: number): string {
  const type = mode & 0o170000;
  const head = type === 0o040000 ? "d" : type === 0o120000 ? "l" : "-";
  let perms = "";
  for (const shift of [6, 3, 0]) {
    perms += (mode >> (shift + 2)) & 1 ? "r" : "-";
    perms += (mode >> (shift + 1)) & 1 ? "w" : "-";
    perms += (mode >> shift) & 1 ? "x" : "-";
  }
  return head + perms;
}

/** Split a byte rate into its numeric value, unit and display precision. */
export function rateParts(bytesPerSec: number): {
  value: number;
  unit: string;
  decimals: number;
} {
  const { div, prefix } = byteScale(bytesPerSec, true);
  const decimals = prefix === "G" ? 2 : prefix === "" ? 0 : 1;
  return { value: bytesPerSec / div, unit: `${prefix}B/s`, decimals };
}

/** Format a byte rate into a numeric value string. */
export function formatRateValue(bytesPerSec: number): string {
  const { value, decimals } = rateParts(bytesPerSec);
  return value.toFixed(decimals);
}

/** Format a byte rate into its unit string. */
export function formatRateUnit(bytesPerSec: number): string {
  return rateParts(bytesPerSec).unit;
}
