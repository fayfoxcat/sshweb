import * as iconv from "iconv-lite";
import { Buffer } from "buffer";

/** Encodings offered in the file editor. */
export const ENCODINGS = [
  "utf-8",
  "gbk",
  "gb2312",
  "big5",
  "shift_jis",
  "utf-16le",
  "utf-16be",
  "latin1",
] as const;
export type Encoding = (typeof ENCODINGS)[number];

/** Server-supported terminal transcodings: the editor encodings minus the
 *  UTF-16 variants (remote terminals must actually run the encoding, and
 *  UTF-16 is not suitable for terminal I/O). Derived from `ENCODINGS` so the
 *  two lists cannot drift. */
export const TERMINAL_ENCODINGS = ENCODINGS.filter(
  (enc) => enc !== "utf-16le" && enc !== "utf-16be",
);

/** Decode bytes into text using the given encoding. */
export function decodeBytes(data: Uint8Array, enc: string): string {
  if (enc === "utf-8") return new TextDecoder("utf-8").decode(data);
  return iconv.decode(Buffer.from(data as unknown as ArrayBuffer), enc);
}

/** Encode text into bytes using the given encoding. */
export function encodeText(text: string, enc: string): Uint8Array {
  if (enc === "utf-8") return new TextEncoder().encode(text);
  return new Uint8Array(iconv.encode(text, enc));
}
