import { writable } from "svelte/store";

import { request } from "./api";
import type { WsServerConfig } from "./protocol";

/** 一个运行中的 SOCKS5 隧道（服务端状态）。 */
export type ProxyStatus = {
  /** 服务器标识 `user@host:port`（与 `serverTargetKey` 一致）。 */
  serverKey: string;
  /** 服务器显示名。 */
  name: string;
  /** 本地监听端口。 */
  port: number;
};

const proxyStore = writable<ProxyStatus[]>([]);

/** 当前运行中的 SOCKS5 隧道（serverKey → 状态）。 */
export const proxies = proxyStore;

/** 刷新隧道列表（服务器面板打开 / 登录后调用）。 */
export async function loadProxies(): Promise<void> {
  const list = await request<ProxyStatus[]>("/api/proxies");
  proxyStore.set(list);
}

/** 开启某服务器的 SOCKS5 隧道。`port` 为 0 时由服务端自动分配。 */
export async function startProxy(
  server: WsServerConfig,
  port = 0,
): Promise<ProxyStatus> {
  const status = await request<ProxyStatus>("/api/proxies", {
    method: "POST",
    body: JSON.stringify({ server, port }),
  });
  proxyStore.update((list) => [
    ...list.filter((p) => p.serverKey !== status.serverKey),
    status,
  ]);
  return status;
}

/** 停止某服务器的 SOCKS5 隧道。 */
export async function stopProxy(serverKey: string): Promise<void> {
  await request<void>(`/api/proxies/${encodeURIComponent(serverKey)}`, {
    method: "DELETE",
  });
  proxyStore.update((list) => list.filter((p) => p.serverKey !== serverKey));
}
