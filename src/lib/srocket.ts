/**
 * @file Internal library for sshweb, providing real-time communication.
 *
 * The contents of this file are technically general, not sshweb-specific, but it
 * is not open-sourced as its own library because it's not ready for that.
 */

import { Decoder, encode, type Options } from "cbor-x";

import { RECONNECT_DELAY_MS, SROCKET_BUFFER_SIZE } from "./constants";

// Rust serializes SFTP sizes and timestamps as u64. Decode CBOR int64 values
// as JavaScript numbers at the protocol boundary so UI arithmetic never mixes
// BigInt with number.
// cbor-x 1.6 supports this runtime option, but its published TypeScript
// declaration does not include it yet.
//
// `useRecords: false` is mandatory: the Decoder constructor only enables
// `mapsAsObjects` when `useRecords === false`. Without it, CBOR maps are
// decoded as Map instances and every `message.hello` / `message.shells`
// property access yields undefined - the app silently ignores all messages
// and the terminal never connects (no error is raised anywhere).
const decoderOptions = {
  int64AsNumber: true,
  useRecords: false,
  mapsAsObjects: true,
} as unknown as Options;
const decoder = new Decoder(decoderOptions);

export type SrocketOptions<T> = {
  /** Handle a message received from the server. */
  onMessage(message: T): void;

  /** Called when the socket connects to the server. */
  onConnect?(): void;

  /** Called when a connected socket is closed. */
  onDisconnect?(): void;

  /** Called when an incoming or existing connection is closed. */
  onClose?(event: CloseEvent): void;
};

/** A reconnecting WebSocket client for real-time communication. */
export class Srocket<T, U> {
  #url: string;
  #options: SrocketOptions<T>;

  #ws: WebSocket | null;
  #connected: boolean;
  #buffer: Uint8Array[];
  #disposed: boolean;

  constructor(url: string, options: SrocketOptions<T>) {
    this.#url = url;
    if (this.#url.startsWith("/")) {
      // Get WebSocket URL relative to the current origin.
      this.#url =
        (window.location.protocol === "https:" ? "wss://" : "ws://") +
        window.location.host +
        this.#url;
    }
    this.#options = options;

    this.#ws = null;
    this.#connected = false;
    this.#buffer = [];
    this.#disposed = false;
    this.#reconnect();
  }

  get connected() {
    return this.#connected;
  }

  /** Queue a message to send to the server, with "at-most-once" semantics. */
  send(message: U) {
    // Types in cbor-x are incorrect here, so cast to fix the error.
    // See: https://github.com/kriszyp/cbor-x/issues/120
    const data = <Uint8Array>(encode(message) as unknown);

    if (this.#connected && this.#ws) {
      this.#ws.send(data);
    } else {
      if (this.#buffer.length < SROCKET_BUFFER_SIZE) {
        this.#buffer.push(data);
      }
    }
  }

  /** Dispose of this WebSocket permanently. */
  dispose() {
    this.#stateChange(false);
    this.#disposed = true;
    this.#ws?.close();
  }

  #reconnect() {
    if (this.#disposed) return;
    if (this.#ws !== null) {
      throw new Error("invariant violation: reconnecting while connected");
    }
    this.#ws = new WebSocket(this.#url);
    this.#ws.binaryType = "arraybuffer";
    this.#ws.onopen = () => {
      this.#stateChange(true);
    };
    this.#ws.onclose = (event) => {
      this.#options.onClose?.(event);
      this.#ws = null;
      this.#stateChange(false);
      setTimeout(() => this.#reconnect(), RECONNECT_DELAY_MS);
    };
    this.#ws.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        const message: T = decoder.decode(new Uint8Array(event.data));
        this.#options.onMessage(message);
      } else {
        console.warn("unexpected non-buffer message, ignoring");
      }
    };
  }

  #stateChange(connected: boolean) {
    if (!this.#disposed && connected !== this.#connected) {
      this.#connected = connected;
      if (connected) {
        this.#options.onConnect?.();

        if (!this.#ws) {
          throw new Error("invariant violation: connected but ws is null");
        }
        // Send any queued messages.
        for (const message of this.#buffer) {
          this.#ws.send(message);
        }
        this.#buffer = [];
      } else {
        this.#options.onDisconnect?.();
      }
    }
  }
}
