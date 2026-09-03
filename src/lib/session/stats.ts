import { writable } from "svelte/store";

import { STATS_POLL_MS } from "../constants";
import type { WsServerConfig } from "../protocol";

/** System statistics for one terminal's server. */
export type StatsData = {
  cpu: number;
  memory: number;
  up: number;
  down: number;
  time: number;
};

/** Accessors for the reactive values that drive the stats poller. The
 *  component reads them at each tick / restart, so a mid-timer change to the
 *  session key or the active shell's server config is picked up without
 *  restarting the timer. */
type StatsParams = {
  sessionName: () => string;
  shellServers: () => Record<number, WsServerConfig | null>;
  activeId: () => number;
  connected: () => boolean;
};

/** Polls `/api/stats` once per second for the active shell. Remote shells
 *  report the stats of their SSH host; local shells keep reporting the machine
 *  running sshweb-server. Only the active shell is polled; the timer restarts
 *  when the active tab or the connection state changes. */
export function createStatsPolling() {
  const byShell = writable<Record<number, StatsData>>({});
  let timer: ReturnType<typeof setInterval> | null = null;

  async function pollOnce(params: StatsParams) {
    const activeId = params.activeId();
    if (activeId < 0) return;
    const sessionName = params.sessionName();
    const shellServers = params.shellServers();
    const url =
      sessionName && shellServers[activeId]
        ? `/api/stats?session=${encodeURIComponent(
            sessionName,
          )}&sid=${activeId}`
        : "/api/stats";
    try {
      const res = await fetch(url);
      if (res.status !== 200) return;
      const data = await res.json();
      byShell.update((m) => ({
        ...m,
        [activeId]: {
          cpu: data.cpu,
          memory: data.memory,
          up: data.up,
          down: data.down,
          time: data.time,
        },
      }));
    } catch (err) {
      // Ignore transient failures; keep the previous values.
    }
  }

  /** Restart the 1s poller for the current active shell (no-op when there is
   *  no active shell or the connection is down). */
  function restart(params: StatsParams) {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
    if (params.activeId() >= 0 && params.connected()) {
      void pollOnce(params);
      timer = setInterval(() => pollOnce(params), STATS_POLL_MS);
    }
  }

  /** Stop the poller (on disconnect / unmount). */
  function stop() {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
  }

  /** Drop stats for shells that no longer exist (closed tabs), so a long
   *  session doesn't accumulate stale records. Only the active shell is
   *  polled, but switching tabs leaves the previous shell's entry behind. */
  function prune(activeIds: number[]) {
    const keep = new Set(activeIds);
    byShell.update((m) => {
      const next: Record<number, StatsData> = {};
      for (const [id, data] of Object.entries(m)) {
        const sid = Number(id);
        if (keep.has(sid)) next[sid] = data;
      }
      return next;
    });
  }

  return { byShell, restart, stop, prune };
}
