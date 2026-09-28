//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { NotificationError, MAX_WIRE_BYTES, protocol, runStream } from "./notification-core.mjs";

const transport = () => new NotificationError("NOTIFICATION_TRANSPORT", { retryable: true });
const turn = () => globalThis.scheduler?.yield?.() ?? new Promise((resolve) => setTimeout(resolve, 0));

function rawHeaders(response) {
  const headers = [];
  let bytes = 0;
  for (const [name, value] of response.headers) {
    bytes += name.length + value.length + 4;
    if (bytes > MAX_WIRE_BYTES) throw protocol("byte_limit");
    headers.push(name, value);
  }
  return headers;
}

function runAttempt(connection, signal, episodeDeadline, onReady, onEvent) {
  const { origin, token, request } = connection;
  let reader;
  async function* chunks() {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      yield value;
    }
  }
  return runStream(connection, signal, episodeDeadline, onReady, onEvent, {
    now: () => performance.now(), turn, error: transport,
    async open(attemptSignal, connected) {
      const response = await globalThis.fetch(new URL("v1/notifications/subscribe", origin), {
        method: "POST",
        headers: { accept: "text/event-stream", "content-type": "application/json", authorization: "Bearer " + token },
        body: request.body,
        mode: "cors", credentials: "omit", cache: "no-store", redirect: "manual", referrerPolicy: "no-referrer",
        signal: attemptSignal,
      });
      // Fetch exposes response headers, not the underlying socket/TLS phases.
      connected();
      if (response.type === "opaqueredirect" || response.redirected) throw protocol("redirect");
      if (response.type === "opaque" || response.status === 0 || response.body === null) throw protocol("invalid_headers");
      reader = response.body.getReader();
      return { statusCode: response.status, rawHeaders: rawHeaders(response), [Symbol.asyncIterator]: chunks };
    },
    async close() {
      if (reader === undefined) return;
      try { await reader.cancel(); }
      catch { /* Abort may already have errored the body; release the same reader. */ }
      finally { reader.releaseLock(); }
    },
  });
}

function randomInt(minimum, maximum) {
  const range = maximum - minimum;
  const ceiling = Math.floor(0x1_0000_0000 / range) * range;
  const word = new Uint32Array(1);
  do { globalThis.crypto.getRandomValues(word); } while (word[0] >= ceiling);
  return minimum + word[0] % range;
}

export const fetchRuntime = Object.freeze({ runAttempt, now: () => performance.now(), randomInt });
