import { writable } from "svelte/store";

import { request } from "./api";
import type { WsServerConfig } from "./protocol";

/** A saved server-side SSH key (public parts only). Private keys never leave
 *  the server — they are stored inside the encrypted config file. */
export type SshKey = {
  id: string;
  name: string;
  publicKey: string;
  fingerprint: string;
};

const keysStore = writable<SshKey[]>([]);

/** Saved SSH keys (public parts only). */
export const keys = keysStore;

/** Load the saved key list from the server. */
export async function loadKeys(): Promise<void> {
  const list = await request<SshKey[]>("/api/keys");
  keysStore.set(list);
}

/** Generate a new Ed25519 keypair on the server and return its public info. */
export async function createKey(name: string): Promise<SshKey> {
  const key = await request<SshKey>("/api/keys", {
    method: "POST",
    body: JSON.stringify({ name }),
  });
  keysStore.update((list) => [...list, key]);
  return key;
}

/** Delete a saved key (servers referencing it will fail auth with a clear
 *  "密钥不存在或已删除" error until re-pointed at another key). */
export async function deleteKey(id: string): Promise<void> {
  await request(`/api/keys/${encodeURIComponent(id)}`, { method: "DELETE" });
  keysStore.update((list) => list.filter((key) => key.id !== id));
}

/** Rename a saved key. Resolves the updated public info. */
export async function renameKey(id: string, name: string): Promise<SshKey> {
  const key = await request<SshKey>(`/api/keys/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: JSON.stringify({ name }),
  });
  keysStore.update((list) => list.map((k) => (k.id === id ? key : k)));
  return key;
}

/** Install a saved key's public part onto the target server. The server
 *  connects with the configuration's own authentication (password or the
 *  selected saved key), so no separate bootstrap credential is required.
 *  Resolves to the server's success message. */
export async function installKey(
  server: WsServerConfig,
  keyId: string,
): Promise<string> {
  const result = await request<{ message: string }>("/api/keys/install", {
    method: "POST",
    body: JSON.stringify({ server, keyId }),
  });
  return result.message;
}
