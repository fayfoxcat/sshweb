/** Generate a UUID v4.
 *
 *  `crypto.randomUUID()` only exists in **secure contexts** (HTTPS or
 *  `localhost`); these sshweb deployments are usually served over plain HTTP
 *  on a LAN IP, where `window.crypto.randomUUID` is `undefined` and calling it
 *  throws `TypeError: crypto.randomUUID is not a function`. Every call site
 *  must go through this helper (not raw `crypto.randomUUID()`), which falls
 *  back to RFC-4122 shaping over `crypto.getRandomValues` (available in every
 *  context) and finally to `Math.random` on exotic environments. */
export function uuid(): string {
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.randomUUID === "function"
  ) {
    return crypto.randomUUID();
  }
  const bytes = new Uint8Array(16);
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.getRandomValues === "function"
  ) {
    crypto.getRandomValues(bytes);
  } else {
    for (let i = 0; i < 16; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  // Set the version (4) and variant (10xx) bits, then format as a UUID.
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0"));
  return (
    hex.slice(0, 4).join("") +
    "-" +
    hex.slice(4, 6).join("") +
    "-" +
    hex.slice(6, 8).join("") +
    "-" +
    hex.slice(8, 10).join("") +
    "-" +
    hex.slice(10).join("")
  );
}
