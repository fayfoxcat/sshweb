import { writable } from "svelte/store";

import { request } from "./api";
import { loadServers } from "./connections";
import { loadKeys } from "./keys";

type AuthStatus = {
  setup: boolean;
  authenticated: boolean;
  /** Session logged in with the one-time setup key; a password must be set
   *  before anything else (首次启动强制改密). */
  pendingChange: boolean;
  loading: boolean;
  error: string | null;
};

export const authStatus = writable<AuthStatus>({
  setup: false,
  authenticated: false,
  pendingChange: false,
  loading: true,
  error: null,
});

async function postPassword(
  path: string,
  password: string,
  confirmation?: string,
  extra: Record<string, string> = {},
): Promise<void> {
  await request<void>(path, {
    method: "POST",
    body: JSON.stringify({ password, confirmation, ...extra }),
  });
}

/** Mark the user authenticated and load the protected server configuration. */
async function finalizeAuth(): Promise<void> {
  authStatus.set({
    setup: true,
    authenticated: true,
    pendingChange: false,
    loading: false,
    error: null,
  });
  await loadServers();
  await loadKeys();
}

/** Refresh setup/authentication state from the server. */
export async function fetchAuthStatus(): Promise<AuthStatus> {
  const result = await request<{
    setup: boolean;
    authenticated: boolean;
    pendingChange?: boolean;
  }>("/api/auth/status");
  const next: AuthStatus = {
    setup: result.setup,
    authenticated: result.authenticated,
    pendingChange: result.pendingChange ?? false,
    loading: false,
    error: null,
  };
  authStatus.set(next);
  if (next.authenticated && !next.pendingChange) {
    await loadServers();
    await loadKeys();
  }
  return next;
}

/** Log in with the page access password — or, on a fresh install, with the
 *  one-time setup key (the server then forces a password change). */
export async function login(password: string): Promise<void> {
  await postPassword("/api/auth/login", password);
  const status = await fetchAuthStatus();
  if (status.authenticated && !status.pendingChange) {
    await finalizeAuth();
  }
}

/** Change the page access password. For a setup-key session this sets the
 *  first password and converts the session to a normal one; for a normal
 *  session the current session stays valid and other sessions are revoked. */
export async function changeAccessPassword(
  oldPassword: string,
  newPassword: string,
  confirmation: string,
): Promise<void> {
  await postPassword("/api/auth/password", newPassword, confirmation, {
    oldPassword,
  });
  const status = await fetchAuthStatus();
  if (status.authenticated && !status.pendingChange) {
    await finalizeAuth();
  }
}

/** Revoke the current session cookie. */
export async function logout(): Promise<void> {
  await request<void>("/api/auth/logout", { method: "POST" });
  authStatus.update((status) => ({
    ...status,
    authenticated: false,
    pendingChange: false,
    loading: false,
  }));
}
