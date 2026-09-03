import { browser } from "$app/environment";
import { get, writable } from "svelte/store";

import { STORAGE_KEY_LANGUAGE } from "../constants";
import { en } from "./en";
import { zh } from "./zh";

export type Lang = "zh-CN" | "en";

/** Stored preference wins; otherwise follow the browser UI language. */
function detect(): Lang {
  if (browser) {
    const saved = localStorage.getItem(STORAGE_KEY_LANGUAGE);
    if (saved === "zh-CN" || saved === "en") return saved;
    return (navigator.language || "").toLowerCase().startsWith("zh")
      ? "zh-CN"
      : "en";
  }
  return "zh-CN";
}

export const lang = writable<Lang>(detect());

/** Switch the UI language (persisted; server-side messages keep their original
 *  Chinese text — see 交接文档 §五 notes). */
export function setLang(next: Lang) {
  lang.set(next);
  if (browser) localStorage.setItem(STORAGE_KEY_LANGUAGE, next);
}

/** Translate `key` for an explicitly given language. */
export function t(
  l: Lang,
  key: string,
  vars?: Record<string, string | number>,
): string {
  const dict: Record<string, string> = l === "en" ? en : zh;
  let s = dict[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}

/** Translate `key` with the currently selected language. */
export function tr(
  key: string,
  vars?: Record<string, string | number>,
): string {
  return t(get(lang), key, vars);
}
