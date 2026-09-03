import { browser } from "$app/environment";

/** Read and JSON-parse a sessionStorage value. `fallback` is returned when the
 *  storage is unavailable (SSR / no entry) or the `parse` callback throws.
 *  Keeps the repeated `sessionStorage` guard + try/catch of the editors,
 *  file-view, panel and draft persistence out of each consumer. */
export function storageGet<T>(
  key: string,
  fallback: T,
  parse: (raw: string) => T,
): T {
  if (!browser) return fallback;
  const raw = sessionStorage.getItem(key);
  if (raw === null) return fallback;
  try {
    return parse(raw);
  } catch {
    return fallback;
  }
}

/** JSON-serialize and write a sessionStorage value; unavailability / quota
 *  errors are swallowed (persistence is best-effort, non-fatal). */
export function storageSet(key: string, value: unknown): void {
  if (!browser) return;
  try {
    sessionStorage.setItem(key, JSON.stringify(value));
  } catch {
    // Quota exceeded / storage unavailable — non-fatal.
  }
}
