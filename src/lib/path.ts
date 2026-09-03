/** Join a POSIX base path and a name (as used by the file manager view). */
export function joinPath(base: string, name: string): string {
  if (base === "/") return `/${name}`;
  return `${base}/${name}`;
}

/** Parent directory of a POSIX path. */
export function parentOf(p: string): string {
  const trimmed = p.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx <= 0) return "/";
  return trimmed.slice(0, idx);
}

/** Last path segment (file / directory name). */
export function basename(p: string): string {
  return p.split("/").pop() || p;
}
