//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { NotificationDecoder } = require("./notification-protocol.js");
const { NotificationError, cancelled, timeout } = require("./notification-error.js");
const { streamHeaders, responseError } = require("./notification-headers.js");

// A runtime owns the request and reader. This owner supplies the same budgets,
// decoder, event ordering and cleanup barrier to native HTTP and browser Fetch.
async function runStream(connection, signal, episodeDeadline, onReady, onEvent, transport) {
  const { request, options } = connection;
  const controller = new AbortController();
  let failure = null;
  let connectTimer, deadlineTimer;
  let idle = null;
  let deadline = Math.min(transport.now() + options.readyTimeoutMs, episodeDeadline ?? Infinity);
  const stop = (error) => { failure ??= error; controller.abort(); };
  const abort = () => stop(cancelled());
  const stage = () => idle === null ? "readiness" : "idle";
  const check = () => {
    if (failure !== null) throw failure;
    if (signal.aborted) throw cancelled();
    if (transport.now() >= deadline) throw timeout(stage());
  };
  const arm = () => {
    clearTimeout(deadlineTimer);
    const tick = () => {
      const remaining = deadline - transport.now();
      if (remaining > 0) deadlineTimer = setTimeout(tick, Math.ceil(remaining));
      else stop(timeout(stage()));
    };
    deadlineTimer = setTimeout(tick, Math.max(1, Math.ceil(deadline - transport.now())));
  };
  try {
    check();
    signal.addEventListener("abort", abort, { once: true });
    connectTimer = setTimeout(() => stop(timeout("connection")), options.connectTimeoutMs);
    arm();
    const incoming = await transport.open(controller.signal, () => clearTimeout(connectTimer), stop);
    check();
    if (incoming.statusCode !== 200) throw await responseError(incoming, options, check);
    const decoder = new NotificationDecoder(request, streamHeaders(incoming), options.maxIdleTimeoutMs);
    const forward = (event) => {
      check();
      if (event.kind === "ready") {
        onReady(event);
        idle = event.idleTimeoutMs;
        deadline = transport.now() + idle;
        arm();
      } else if (event.kind === "notification") onEvent(event);
      else if (event.kind === "error") throw event.error;
      else if (event.kind === "closed") throw new NotificationError("NOTIFICATION_SERVER_DRAINING", { retryable: true });
    };
    for await (const chunk of incoming) {
      check();
      if (chunk.length > options.maxTransportChunkBytes) throw new NotificationError("NOTIFICATION_CAPACITY");
      if (idle !== null && chunk.length !== 0) { deadline = transport.now() + idle; arm(); }
      for (let offset = 0; offset < chunk.length;) {
        check();
        const step = decoder.decode(chunk.subarray(offset));
        offset += step.consumed;
        if (step.event !== null) {
          try { forward(step.event); }
          catch (error) {
            if (decoder.isTerminal && offset < chunk.length) decoder.decode(chunk.subarray(offset));
            throw error;
          }
          await transport.turn();
        }
      }
    }
    check();
    for (;;) {
      const event = decoder.finish();
      if (event === null) throw transport.error();
      forward(event);
    }
  } catch (error) { throw failure ?? (error instanceof NotificationError ? error : transport.error(error)); }
  finally {
    clearTimeout(connectTimer);
    clearTimeout(deadlineTimer);
    signal.removeEventListener("abort", abort);
    controller.abort();
    await transport.close();
  }
}

module.exports = { runStream };
