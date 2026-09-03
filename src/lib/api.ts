import { tr } from "./i18n";

/** Whether a response carries a JSON body (API responses do; the SPA fallback
 *  serving `spa.html` for unknown routes does not). */
function isJson(response: Response): boolean {
  return (response.headers.get("content-type") ?? "").includes(
    "application/json",
  );
}

/** Parse an API error body into a readable message. */
export async function parseError(response: Response): Promise<string> {
  if (!isJson(response)) {
    // A non-JSON error response usually means an unknown `/api/*` route fell
    // through to the SPA fallback — typically the frontend and the server are
    // out of sync (stale server binary). Surface that instead of a confusing
    // "Unexpected token '<'" JSON parse error.
    return tr("api.invalidResponse");
  }
  try {
    const body = (await response.json()) as { error?: string };
    return body.error || tr("api.requestFailed", { status: response.status });
  } catch {
    return tr("api.requestFailed", { status: response.status });
  }
}

/** JSON request helper with same-origin credentials. 204 yields undefined. */
export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: "same-origin",
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) throw new Error(await parseError(response));
  if (response.status === 204) return undefined as T;
  if (!isJson(response)) {
    // Successful but non-JSON (SPA fallback for a missing API route).
    throw new Error(tr("api.invalidResponse"));
  }
  return (await response.json()) as T;
}
