/** Link-quality measurement for the nav bar's connection light.
 *
 *  A keystroke travels two hops: **browser → sshweb-server** (measured here)
 *  and **sshweb-server → SSH target** (measured server-side with a
 *  `keepalive@openssh.com` ping, see `runner::probe_remote_latency`). The light
 *  grades their sum — that is the number the user actually feels.
 *
 *  Why two hops and not just one: on a fast local network the browser hop is
 *  ~1 ms even when the SSH target is on another continent, so a
 *  browser-only light would stay green while the terminal lags. And the SSH hop
 *  alone cannot be measured for a local terminal (there is no second hop), so
 *  the browser hop is what keeps that case meaningful.
 */

import { writable } from "svelte/store";

import { LATENCY_PING_MS } from "../constants";

/** Wire round trip the light reports, in milliseconds. */
export type LinkState = {
  /** browser → sshweb-server round trip (null until the first pong). */
  serverMs: number | null;
  /** sshweb-server → SSH target round trip for the active shell. `null` for a
   *  local shell (no second hop) or before the first measurement. */
  remoteMs: number | null;
  /** `serverMs + remoteMs` (each already the minimum of its window). `null`
   *  while nothing is known yet. Ungraded rounding: the grade below uses this
   *  number, the tooltip rounds the two parts. */
  totalMs: number | null;
  /** Grade of `totalMs`; `"down"` whenever the WebSocket is disconnected. */
  quality: LinkQuality;
};

/** Green / yellow / orange / red. */
export type LinkQuality = "good" | "fair" | "poor" | "bad" | "down";

/** Grade boundaries on the **total** round trip (ms).
 *
 *  Chosen against what a terminal feels like: below ~80 ms typing is local, by
 *  200 ms it is noticeable but fine, past 500 ms every keystroke visibly lags
 *  and a shell starts to feel broken. The gaps are wide on purpose — a light
 *  that changes colour for a 5 ms jitter is noise, not information (and the
 *  min-of-window below already removes most of it). */
export const LINK_THRESHOLDS = {
  good: 80,
  fair: 200,
  poor: 500,
} as const;

/** Grade a total round trip. `null` (nothing measured yet) is reported as
 *  `"good"`: an unreported link is not evidence of a bad one, and the light
 *  must not start out red on a healthy page. */
export function gradeLink(totalMs: number | null): LinkQuality {
  if (totalMs === null) return "good";
  if (totalMs < LINK_THRESHOLDS.good) return "good";
  if (totalMs < LINK_THRESHOLDS.fair) return "fair";
  if (totalMs < LINK_THRESHOLDS.poor) return "poor";
  return "bad";
}

/** How many recent samples a hop's displayed value is taken from.
 *
 *  The *minimum* of the last few round trips, not their mean — the same reason
 *  `ping` reports min/avg/max: any extra millisecond beyond the link's own is
 *  queueing somewhere (a page still hydrating, a GC pause, the browser's timer
 *  granularity after being throttled), and averaging those in grades the
 *  browser instead of the network. A genuinely degraded link raises *every*
 *  sample, so the minimum follows it — five samples is ~5 s. */
const SAMPLE_WINDOW = 5;

/** Floor applied before rounding **for display only**. A loopback SSH hop
 *  measures 0.3–0.9 ms, which `Math.round` would print as `0 ms` — reading as
 *  "no hop at all", the exact opposite of what a sub-millisecond hop means. The
 *  unrounded value still feeds the grade, so this changes text, not judgement. */
const DISPLAY_FLOOR_MS = 1;

/** Fold one sample into a hop's window and return the value to display. */
function fold(window: number[], next: number): number {
  window.push(next);
  if (window.length > SAMPLE_WINDOW) window.shift();
  return Math.min(...window);
}

/** Round a hop for the tooltip, never below {@link DISPLAY_FLOOR_MS}. */
export function displayMs(ms: number): number {
  return Math.max(DISPLAY_FLOOR_MS, Math.round(ms));
}

/** Give up on an unanswered probe after this long (ms) and measure again.
 *
 *  Only a lost reply reaches this — a healthy one resolves in milliseconds — so
 *  it exists to keep the probe from stalling, not to bound the round trip. Well
 *  under a minute: the light should recover on its own after a hiccup rather
 *  than freeze on an old number. */
const STALE_PROBE_MS = 8000;

/** Round trip measurement state for one page. */
export function createLinkQuality() {
  const store = writable<LinkState>({
    serverMs: null,
    remoteMs: null,
    totalMs: null,
    quality: "good",
  });

  /** Recent samples per hop; the displayed value is the minimum of the window
   *  (see [`fold`]). Kept outside the store so a sample only has to name the
   *  hop it updates. */
  const serverWindow: number[] = [];
  const remoteWindows = new Map<number, number[]>();
  /** Displayed values (min of each window), `null` until a first sample. */
  let serverMs: number | null = null;
  const remoteMsBySid = new Map<number, number | null>();
  let remoteSid: number | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;
  /** The single in-flight probe: its stamp and when it was sent. */
  let inflight: { stamp: number; at: number } | null = null;
  let send: (() => void) | null = null;
  let visible = true;

  function publish(connected: boolean) {
    const shownRemote =
      remoteSid === null ? null : (remoteMsBySid.get(remoteSid) ?? null);
    // A local shell has no second hop: treat it as 0 so the light grades the
    // hop it does have, instead of falling back to "unknown" forever.
    const remotePart = remoteSid === null ? null : (shownRemote ?? 0);
    const total = serverMs === null ? null : serverMs + (remotePart ?? 0);
    store.set({
      serverMs,
      remoteMs: shownRemote,
      totalMs: total,
      quality: connected ? gradeLink(total) : "down",
    });
  }

  /** One probe tick. `force` sends even from a hidden tab: the *first* sample must
   *  not depend on visibility. A page opened in the background (restored tab,
   *  headless test, another window) otherwise never measures anything at all,
   *  because every scheduled tick is skipped — and if no `visibilitychange`
   *  follows, the light stays "unknown" forever even though the socket is up.
   *  Scheduled ticks stay skipped while hidden: there a sample measures the
   *  browser's 1/minute timer throttle, not the link. */
  function tick(force = false) {
    if (!send) return;
    if (!visible && !force) return;
    const now = performance.now();
    // A probe per second, but **one in flight at a time**: if the previous ping
    // has not come back yet, this tick is skipped rather than queued. Otherwise
    // a late reply resolves an older stamp and the reported round trip grows by
    // a multiple of the interval (the "2000 ms but it feels fine" symptom);
    // `STALE_PROBE_MS` releases the slot when a reply never arrives at all.
    if (inflight !== null && now - inflight.at < STALE_PROBE_MS) return;
    inflight = { stamp: now, at: now };
    send();
  }

  return {
    store,

    /** Start probing. `ping` sends one `Ping` carrying `performance.now()`;
     *  the caller supplies it because the socket lives in the component. */
    start(ping: () => void) {
      // `stop()` first: it clears the sender and the in-flight probe, so
      // calling it after assigning them would leave the probe dead (and
      // silently so — `tick()` just returns).
      this.stop();
      send = ping;
      visible = document.visibilityState !== "hidden";
      timer = setInterval(tick, LATENCY_PING_MS);
      // Force the first sample: see `tick`.
      tick(true);
    },

    stop() {
      if (timer) {
        clearInterval(timer);
        timer = null;
      }
      inflight = null;
      send = null;
    },

    /** A `pong` arrived. Only the one probe in flight can be resolved; a stray
     *  pong (another tab's, or a duplicate) is ignored so it cannot pair with a
     *  newer stamp and produce a bogus round trip. */
    onPong() {
      const probe = inflight;
      if (probe === null) return;
      inflight = null;
      serverMs = fold(serverWindow, performance.now() - probe.stamp);
      publish(true);
    },

    /** The active shell changed: grade `sid`'s SSH hop (its last known value is
     *  reused, so switching tabs does not blank the light). */
    setActiveShell(sid: number | null) {
      remoteSid = sid;
      publish(true);
    },

    /** A `latency` message arrived for `sid`. Stored per shell so switching
     *  back to a tab shows its own hop without waiting a second. */
    onLatencyRemote(sid: number, ms: number | null) {
      if (ms === null) {
        // A timed-out probe: drop the window so the displayed value does not
        // keep a stale good number from before the outage.
        remoteWindows.delete(sid);
        remoteMsBySid.set(sid, null);
      } else {
        const window = remoteWindows.get(sid) ?? [];
        remoteWindows.set(sid, window);
        remoteMsBySid.set(sid, fold(window, ms));
      }
      if (sid === remoteSid) publish(true);
    },

    /** Connection up/down: down must show immediately (that is the whole point
     *  of the light), and the windows are dropped so a reconnect does not
     *  inherit a stale grade. */
    setConnected(connected: boolean) {
      if (!connected) {
        serverWindow.length = 0;
        serverMs = null;
        remoteWindows.clear();
        remoteMsBySid.clear();
        inflight = null;
      }
      publish(connected);
    },

    /** Track tab visibility (see `tick`). */
    onVisibilityChange() {
      visible = document.visibilityState !== "hidden";
      if (visible) tick(true);
    },
  };
}
