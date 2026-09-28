//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const http = require("node:http");
const https = require("node:https");
const { performance } = require("node:perf_hooks");
const { NotificationDecoder } = require("./notification-protocol.js");
const { NotificationError, protocol, cancelled, timeout } = require("./notification-error.js");
const { streamHeaders, responseError } = require("./notification-headers.js");
const { MAX_WIRE_BYTES } = require("./notification-request.js");

function transport(error) {
  if (error instanceof NotificationError) return error;
  const code = typeof error?.code === "string" ? error.code : "";
  if (code.startsWith("HPE_")) return protocol("http_parse");
  return new NotificationError(code.startsWith("ERR_INVALID") ? "NOTIFICATION_INVALID_REQUEST" : "NOTIFICATION_TRANSPORT",
    { retryable: !code.startsWith("ERR_INVALID") });
}
const turn = () => new Promise((resolve) => setImmediate(resolve));

async function runAttempt(connection, signal, episodeDeadline, onReady, onEvent) {
  const { origin, token, request, options } = connection;
  let outgoing, incoming, socket;
  let requestClosed, responseClosed, socketClosed;
  let failure = null;
  let connectTimer, deadlineTimer;
  let idle = null;
  let deadline = Math.min(performance.now() + options.readyTimeoutMs, episodeDeadline ?? Infinity);
  const destroy = () => { incoming?.destroy(); outgoing?.destroy(); socket?.destroy(); };
  const stop = (error) => { failure ??= error; destroy(); };
  const abort = () => stop(cancelled());
  const stage = () => idle === null ? "readiness" : "idle";
  const check = () => {
    if (failure !== null) throw failure;
    if (signal.aborted) throw cancelled();
    if (performance.now() >= deadline) throw timeout(stage());
  };
  const arm = () => {
    clearTimeout(deadlineTimer);
    const tick = () => {
      const remaining = deadline - performance.now();
      if (remaining > 0) deadlineTimer = setTimeout(tick, Math.ceil(remaining));
      else stop(timeout(stage()));
    };
    deadlineTimer = setTimeout(tick, Math.max(1, Math.ceil(deadline - performance.now())));
  };
  try {
    check();
    signal.addEventListener("abort", abort, { once: true });
    incoming = await new Promise((resolve, reject) => {
      outgoing = (origin.protocol === "https:" ? https : http).request(new URL("v1/notifications/subscribe", origin), {
        method: "POST", agent: false, rejectUnauthorized: true, maxHeaderSize: MAX_WIRE_BYTES,
        headers: { accept: "text/event-stream", "accept-encoding": "identity",
          "content-type": "application/json", "content-length": Buffer.byteLength(request.body) },
      }, (response) => {
        incoming = response;
        responseClosed = new Promise((done) => response.once("close", done));
        response.on("error", (error) => { failure ??= transport(error); });
        resolve(response);
      });
      requestClosed = new Promise((done) => outgoing.once("close", done));
      // The byte ceiling already bounds header storage. Do not let Node omit
      // later headers before duplicate and identity validation can see them.
      outgoing.maxHeadersCount = 0;
      outgoing.on("error", (error) => { failure ??= transport(error); reject(failure); });
      outgoing.once("close", () => { if (!incoming) reject(failure ?? transport()); });
      outgoing.once("upgrade", (response) => {
        incoming = response;
        responseClosed = new Promise((done) => response.once("close", done));
        response.on("error", () => {});
        stop(protocol("http_upgrade"));
        reject(failure);
      });
      outgoing.once("socket", (value) => {
        socket = value;
        socketClosed = new Promise((done) => socket.once("close", done));
        const connected = () => clearTimeout(connectTimer);
        if (!socket.connecting && origin.protocol !== "https:") connected();
        else socket.once(origin.protocol === "https:" ? "secureConnect" : "connect", connected);
        if (failure !== null || signal.aborted) socket.destroy();
      });
      connectTimer = setTimeout(() => stop(timeout("connection")), options.connectTimeoutMs);
      arm();
      // Keep credentials out of connection options retained by Node's agent.
      outgoing.setHeader("authorization", "Bearer " + token);
      outgoing.end(request.body);
    });
    check();
    if (incoming.statusCode !== 200) throw await responseError(incoming, options, check);
    const decoder = new NotificationDecoder(request, streamHeaders(incoming), options.maxIdleTimeoutMs);
    const forward = (event) => {
      check();
      if (event.kind === "ready") {
        onReady(event);
        idle = event.idleTimeoutMs;
        deadline = performance.now() + idle;
        arm();
      } else if (event.kind === "notification") onEvent(event);
      else if (event.kind === "error") throw event.error;
      else if (event.kind === "closed") throw new NotificationError("NOTIFICATION_SERVER_DRAINING", { retryable: true });
    };
    for await (const chunk of incoming) {
      check();
      if (chunk.length > options.maxTransportChunkBytes) throw new NotificationError("NOTIFICATION_CAPACITY");
      if (idle !== null && chunk.length !== 0) { deadline = performance.now() + idle; arm(); }
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
          await turn();
        }
      }
    }
    check();
    for (;;) {
      const event = decoder.finish();
      if (event === null) throw transport();
      forward(event);
    }
  } catch (error) { throw error instanceof NotificationError ? error : failure ?? transport(error); }
  finally {
    clearTimeout(connectTimer);
    clearTimeout(deadlineTimer);
    signal.removeEventListener("abort", abort);
    destroy();
    // A destroyed flag alone is not completion. Join each actual close event
    // before the worker can start another request or return from close().
    await requestClosed;
    await responseClosed;
    await socketClosed;
  }
}

module.exports = { runAttempt, cancelled, timeout };
