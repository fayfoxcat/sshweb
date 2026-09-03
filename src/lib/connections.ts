import { get, writable } from "svelte/store";

import { request } from "./api";
import { DEFAULT_SSH_PORT, DEFAULT_SOCKS_PORT } from "./constants";
import { tr } from "./i18n";
import { clearLegacyServers, readLegacyServers } from "./legacy";
import type {
  WsJumpHost,
  WsProxyConfig,
  WsServerConfig,
  WsSocks5Tunnel,
} from "./protocol";

/** A saved SSH configuration held in memory after server-side authentication.
 *  `WsServerConfig` already requires the base connection fields; only the
 *  id + persisted defaults are layered on top. */
export type ServerConfig = WsServerConfig & {
  id: string;
  encoding: string;
  hosts: WsJumpHost[];
  proxy: WsProxyConfig | null;
  macs: string[];
  authMethod: string;
};

/** Form / wire input for creating or updating a server: the saved config
 *  minus the id, with the form-only fields required. Derived from
 *  `ServerConfig` so adding a persisted field cannot silently drift between
 *  the two shapes. */
export type ServerInput = Omit<ServerConfig, "id"> & {
  startup: string;
  keyId: string | null;
};

export type ServerSettings = {
  servers: ServerConfig[];
};

const serverStore = writable<ServerSettings>({ servers: [] });

/** Authenticated server configurations. No SSH secrets are persisted in the browser. */
export const servers = serverStore;

/** Normalize a partial server shape into a full `ServerConfig`. */
function serverConfig(values: {
  id: string;
  name?: string;
  host: string;
  port?: number;
  username: string;
  password?: string;
  encoding?: string;
  hosts?: WsJumpHost[];
  proxy?: WsProxyConfig | null;
  macs?: string[];
  startup?: string;
  authMethod?: string;
  keyId?: string | null;
  socks5Tunnel?: WsSocks5Tunnel | null;
}): ServerConfig {
  return {
    id: values.id,
    name: values.name || values.host,
    host: values.host,
    port: Number(values.port) || DEFAULT_SSH_PORT,
    username: values.username,
    password: values.password ?? "",
    encoding: values.encoding || "utf-8",
    hosts: values.hosts ?? [],
    proxy: values.proxy ?? null,
    macs: values.macs ?? [],
    startup: values.startup ?? "",
    authMethod: values.authMethod || "password",
    keyId: values.keyId ?? undefined,
    socks5Tunnel: values.socks5Tunnel ?? undefined,
  };
}

/** Strip the browser-side id, producing the WebSocket wire format. Accepts a
 *  saved `ServerConfig` (with id) or a form/input shape without one. */
export function toWsServerConfig(
  server: WsServerConfig & { id?: string },
): WsServerConfig {
  const rest = { ...server } as Partial<ServerConfig>;
  delete rest.id;
  return rest as WsServerConfig;
}

/** Stable identity of a server for the file-manager view binding:
 *  `user@host:port`, or `"local"` for a local shell. All terminals of one
 *  server share a single SFTP view (server-side the browse target is a
 *  per-server headless shell), so the view is keyed by server, not by sid. */
export function serverTargetKey(server: WsServerConfig | null): string {
  if (!server) return "local";
  return `${server.username}@${server.host}:${server.port}`;
}

/** Flat form fields for the outbound proxy (the `proxy` object split one
 *  field per input; reassembled by [`joinProxy`]). */
export type ProxyFormFields = {
  proxyEnabled: boolean;
  proxyKind: "http" | "socks5";
  proxyHost: string;
  proxyPort: number;
  proxyUser: string;
  proxyPass: string;
};

/** Flat form fields for the SOCKS5 tunnel (inbound; see [`joinSocks5Tunnel`]). */
export type Socks5FormFields = {
  socks5Enabled: boolean;
  socks5Port: number;
  socks5User: string;
  socks5Pass: string;
};

/** Split a saved `proxy` object into the flat form fields (single source for
 *  the blank / edit form; null → all fields at their defaults). */
export function splitProxy(proxy: WsProxyConfig | null): ProxyFormFields {
  return {
    proxyEnabled: !!proxy,
    proxyKind: proxy?.kind ?? "http",
    proxyHost: proxy?.host ?? "",
    proxyPort: proxy?.port ?? DEFAULT_SOCKS_PORT,
    proxyUser: proxy?.username ?? "",
    proxyPass: proxy?.password ?? "",
  };
}

/** Split a saved `socks5Tunnel` into the flat form fields. */
export function splitSocks5Tunnel(
  tunnel: WsSocks5Tunnel | null | undefined,
): Socks5FormFields {
  return {
    socks5Enabled: !!tunnel,
    socks5Port: tunnel?.port ?? DEFAULT_SOCKS_PORT,
    socks5User: tunnel?.username ?? "",
    socks5Pass: tunnel?.password ?? "",
  };
}

/** Reassemble the flat proxy fields into a `WsProxyConfig`, or null when
 *  disabled. Ports are normalized (empty / 0 / invalid fall back to the
 *  default, avoiding NaN serialization errors). */
export function joinProxy(flat: ProxyFormFields): WsProxyConfig | null {
  if (!flat.proxyEnabled) return null;
  return {
    kind: flat.proxyKind,
    host: flat.proxyHost,
    port: Number(flat.proxyPort) || DEFAULT_SOCKS_PORT,
    username: flat.proxyUser,
    password: flat.proxyPass,
  };
}

/** Reassemble the flat SOCKS5 fields into a `WsSocks5Tunnel`, or undefined
 *  when disabled. Ports are normalized like [`joinProxy`]. */
export function joinSocks5Tunnel(
  flat: Socks5FormFields,
): WsSocks5Tunnel | undefined {
  if (!flat.socks5Enabled) return undefined;
  return {
    port: Number(flat.socks5Port) || DEFAULT_SOCKS_PORT,
    username: flat.socks5User,
    password: flat.socks5Pass,
  };
}

/** The effective SSH password: the form's value, falling back to the saved
 *  server's when editing (a blank form password preserves the saved one).
 *  Shared by `updateServer` and the form's test/install flows. */
export function effectivePassword(inputPwd: string, savedPwd: string): string {
  return inputPwd || savedPwd;
}

function normalizeSettings(value: {
  servers?: Array<WsServerConfig & { id: string }>;
}): ServerSettings {
  const configs = Array.isArray(value.servers)
    ? value.servers.map(serverConfig)
    : [];
  return { servers: configs };
}

async function persist(next: ServerSettings): Promise<void> {
  await request<void>("/api/config", {
    method: "PUT",
    body: JSON.stringify(next),
  });
  serverStore.set(next);
}

/** Load the authenticated configuration.
 *
 *  One-time migration: when the server store is still empty and the old
 *  browser store (`sshx-servers-store`) still holds servers, they are
 *  decrypted and imported via `/api/config/import` before the key is cleared.
 *  Otherwise the server-side settings are used as-is. */
export async function loadServers(): Promise<void> {
  const remote = await request<{
    servers?: Array<WsServerConfig & { id: string }>;
  }>("/api/config");
  if (remote.servers?.length) {
    clearLegacyServers();
    serverStore.set(normalizeSettings(remote));
    return;
  }
  const legacy = await readLegacyServers();
  if (legacy) {
    const imported = await request<{
      servers?: Array<WsServerConfig & { id: string }>;
    }>("/api/config/import", {
      method: "POST",
      body: JSON.stringify({
        servers: legacy.map((s) => serverConfig({ ...s })),
      }),
    });
    clearLegacyServers();
    serverStore.set(normalizeSettings(imported));
    return;
  }
  // No remote servers and no legacy store: leave the (already-empty) store.
  clearLegacyServers();
}

/** Add and persist a server configuration. */
export async function addServer(input: ServerInput): Promise<ServerConfig> {
  const current = get(serverStore);
  const config = serverConfig({ id: crypto.randomUUID(), ...input });
  await persist({
    ...current,
    servers: [...current.servers, config],
  });
  return config;
}

/** Update a server. An empty password preserves the current SSH password. */
export async function updateServer(
  id: string,
  input: ServerInput,
): Promise<void> {
  const current = get(serverStore);
  const existing = current.servers.find((server) => server.id === id);
  if (!existing) throw new Error(tr("session.serverNotFound"));
  const updated = serverConfig({
    id,
    name: input.name || input.host,
    host: input.host,
    port: input.port,
    username: input.username,
    password: effectivePassword(input.password, existing.password),
    encoding: input.encoding || existing.encoding,
    hosts: input.hosts ?? existing.hosts,
    proxy: input.proxy,
    macs: input.macs ?? existing.macs,
    startup: input.startup ?? existing.startup,
    authMethod: input.authMethod || existing.authMethod || "password",
    keyId: input.keyId ?? null,
    socks5Tunnel: input.socks5Tunnel,
  });
  await persist({
    ...current,
    servers: current.servers.map((server) =>
      server.id === id ? updated : server,
    ),
  });
}

/** Delete a server. */
export async function deleteServer(id: string): Promise<void> {
  const current = get(serverStore);
  await persist({
    servers: current.servers.filter((server) => server.id !== id),
  });
}

/** Duplicate a server, appending a visible copy suffix. */
export async function duplicateServer(
  id: string,
): Promise<ServerConfig | null> {
  const current = get(serverStore);
  const source = current.servers.find((server) => server.id === id);
  if (!source) return null;
  const copy: ServerConfig = {
    ...source,
    id: crypto.randomUUID(),
    name: `${source.name}${tr("common.copySuffix")}`,
    hosts: source.hosts.map((host) => ({ ...host })),
    proxy: source.proxy ? { ...source.proxy } : null,
  };
  await persist({ ...current, servers: [...current.servers, copy] });
  return copy;
}

/** Reorder the saved server list. */
export async function moveServer(fromId: string, toId: string): Promise<void> {
  const current = get(serverStore);
  const from = current.servers.findIndex((server) => server.id === fromId);
  const to = current.servers.findIndex((server) => server.id === toId);
  if (from === -1 || to === -1 || from === to) return;
  const reordered = [...current.servers];
  const [item] = reordered.splice(from, 1);
  reordered.splice(to, 0, item);
  await persist({ ...current, servers: reordered });
}

/** Test an (possibly unsaved) server configuration by having the server open an
 *  SSH transport connection and authenticate. Resolves to the server's message
 *  on success; throws with a readable reason on failure. */
export async function testServerConnection(
  server: ServerInput,
): Promise<string> {
  const result = await request<{ message: string }>("/api/test-connection", {
    method: "POST",
    body: JSON.stringify({ server }),
  });
  return result.message;
}
