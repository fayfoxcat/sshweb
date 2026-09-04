/**
 * @file One-time migration of the pre-server-side browser store
 * (`sshx-servers-store`).
 *
 * The old release kept SSH settings in localStorage with the passwords
 * encrypted by AES-GCM using a fixed client secret (which is public, so the
 * "encryption" only protected casual reading). Server-side encrypted config
 * superseded it; this module imports the old contents exactly once, when the
 * server store is still empty, and then clears the legacy key.
 */

import { browser } from "$app/environment";

import { STORAGE_KEY_SERVERS } from "./constants";
import type { WsServerConfig } from "./protocol";
import { uuid } from "./uuid";

const SECRET = "sshx-local-secret-v1";
const SALT = "sshx-servers-salt";
const IV_LENGTH = 12;

/** A raw legacy entry as stored in localStorage: the same connection fields
 *  as `WsServerConfig`, but the password is the old AES-GCM ciphertext. */
type RawLegacyServer = Omit<
  WsServerConfig,
  "password" | "startup" | "authMethod" | "keyId"
> & { id: string; passwordCipher: string };

/** A legacy entry with its password decrypted (kept flat; the caller applies
 *  the current `ServerConfig` defaults). */
type LegacyServer = Omit<WsServerConfig, "startup" | "authMethod" | "keyId"> & {
  id: string;
};

async function getKey(): Promise<CryptoKey> {
  const enc = new TextEncoder();
  const keyMaterial = await crypto.subtle.importKey(
    "raw",
    enc.encode(SECRET),
    "PBKDF2",
    false,
    ["deriveKey"],
  );
  return crypto.subtle.deriveKey(
    {
      name: "PBKDF2",
      salt: enc.encode(SALT),
      iterations: 100_000,
      hash: "SHA-256",
    },
    keyMaterial,
    { name: "AES-GCM", length: 256 },
    false,
    ["decrypt"],
  );
}

function base64ToBytes(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/** Decrypt a legacy base64-encoded ciphertext. */
async function decryptSecret(payload: string): Promise<string> {
  const key = await getKey();
  const buf = base64ToBytes(payload);
  const iv = buf.slice(0, IV_LENGTH);
  const ct = buf.slice(IV_LENGTH);
  const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv }, key, ct);
  return new TextDecoder().decode(pt);
}

/** Read and decrypt the legacy store. Returns `null` when absent/invalid. */
export async function readLegacyServers(): Promise<LegacyServer[] | null> {
  if (!browser) return null;
  const raw = localStorage.getItem(STORAGE_KEY_SERVERS);
  if (!raw) return null;
  try {
    const legacy = JSON.parse(raw) as { servers?: RawLegacyServer[] };
    if (!Array.isArray(legacy.servers) || legacy.servers.length === 0) {
      return null;
    }
    return Promise.all(
      // Defaults for `name`/`port` are applied later by `serverConfig` in
      // connections.ts — don't re-normalize here.
      legacy.servers.map(async (server) => ({
        id: server.id || uuid(),
        name: server.name,
        host: server.host,
        port: server.port,
        username: server.username,
        password: await decryptSecret(server.passwordCipher),
        encoding: server.encoding,
        hosts: server.hosts,
        proxy: server.proxy,
        macs: server.macs,
      })),
    );
  } catch (error) {
    console.warn("legacy server configuration migration failed", error);
    return null;
  }
}

/** Drop the legacy key; called once after a successful import (or when the
 *  server store already has servers — see `loadServers`). */
export function clearLegacyServers(): void {
  if (browser) localStorage.removeItem(STORAGE_KEY_SERVERS);
}
