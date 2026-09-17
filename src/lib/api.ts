import { tr } from "./i18n";

/** Whether a response carries a JSON body (API responses do; the SPA fallback
 *  — the embedded `index.html` shell served for unknown routes — does not). */
function isJson(response: Response): boolean {
  return (response.headers.get("content-type") ?? "").includes(
    "application/json",
  );
}

/** An API error carrying the parsed JSON body alongside the message.
 *
 *  The message is all most callers need, but some failures come with structured
 *  detail the text alone cannot convey — a host-key change carries both
 *  fingerprints (已知坑 80). Extends `Error`, so `instanceof Error`,
 *  `.message` and `toastError` keep working unchanged. */
export class ApiError extends Error {
  readonly body: Record<string, unknown>;

  constructor(message: string, body: Record<string, unknown> = {}) {
    super(message);
    this.name = "ApiError";
    this.body = body;
  }
}

/** The message and raw body of a failed response. */
async function readError(
  response: Response,
): Promise<{ message: string; body: Record<string, unknown> }> {
  if (!isJson(response)) {
    // A non-JSON error response usually means an unknown `/api/*` route fell
    // through to the SPA fallback — typically the frontend and the server are
    // out of sync (stale server binary). Surface that instead of a confusing
    // "Unexpected token '<'" JSON parse error.
    return { message: tr("api.invalidResponse"), body: {} };
  }
  const fallback = tr("api.requestFailed", { status: response.status });
  try {
    const body = (await response.json()) as Record<string, unknown>;
    const error = body.error;
    return {
      message: typeof error === "string" && error ? error : fallback,
      body,
    };
  } catch {
    return { message: fallback, body: {} };
  }
}

/** Parse an API error body into a readable message. */
export async function parseError(response: Response): Promise<string> {
  return (await readError(response)).message;
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
  if (!response.ok) {
    const { message, body } = await readError(response);
    throw new ApiError(message, body);
  }
  if (response.status === 204) return undefined as T;
  if (!isJson(response)) {
    // Successful but non-JSON (SPA fallback for a missing API route).
    throw new Error(tr("api.invalidResponse"));
  }
  return (await response.json()) as T;
}
