//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const http = require("node:http");
const https = require("node:https");
const { performance } = require("node:perf_hooks");
const { NotificationError, protocol, cancelled, timeout } = require("./notification-error.js");
const { MAX_WIRE_BYTES } = require("./notification-request.js");
const { runStream } = require("./notification-stream.js");

function transport(error) {
  if (error instanceof NotificationError) return error;
  const code = typeof error?.code === "string" ? error.code : "";
  if (code.startsWith("HPE_")) return protocol("http_parse");
  return new NotificationError(code.startsWith("ERR_INVALID") ? "NOTIFICATION_INVALID_REQUEST" : "NOTIFICATION_TRANSPORT",
    { retryable: !code.startsWith("ERR_INVALID") });
}

function runAttempt(connection, signal, episodeDeadline, onReady, onEvent) {
  const { origin, token, request } = connection;
  let outgoing, incoming, socket, requestClosed, responseClosed, socketClosed, activeSignal;
  const destroy = () => { incoming?.destroy(); outgoing?.destroy(); socket?.destroy(); };
  return runStream(connection, signal, episodeDeadline, onReady, onEvent, {
    now: () => performance.now(),
    turn: () => new Promise((resolve) => setImmediate(resolve)),
    error: transport,
    open(attemptSignal, connected, failed) {
      activeSignal = attemptSignal;
      attemptSignal.addEventListener("abort", destroy, { once: true });
      return new Promise((resolve, reject) => {
        let failure;
        const fail = (error) => { failure ??= transport(error); failed(failure); reject(failure); };
        outgoing = (origin.protocol === "https:" ? https : http).request(new URL("v1/notifications/subscribe", origin), {
          method: "POST", agent: false, rejectUnauthorized: true, maxHeaderSize: MAX_WIRE_BYTES,
          headers: { accept: "text/event-stream", "accept-encoding": "identity",
            "content-type": "application/json", "content-length": Buffer.byteLength(request.body) },
        }, (response) => {
          incoming = response;
          responseClosed = new Promise((done) => response.once("close", done));
          response.on("error", fail);
          resolve(response);
        });
        requestClosed = new Promise((done) => outgoing.once("close", done));
        // The byte ceiling bounds storage; duplicate validation needs all headers.
        outgoing.maxHeadersCount = 0;
        outgoing.on("error", fail);
        outgoing.once("close", () => { if (!incoming) reject(failure ?? transport()); });
        outgoing.once("upgrade", (response) => {
          incoming = response;
          responseClosed = new Promise((done) => response.once("close", done));
          response.on("error", () => {});
          fail(protocol("http_upgrade"));
        });
        outgoing.once("socket", (value) => {
          socket = value;
          socketClosed = new Promise((done) => socket.once("close", done));
          if (!socket.connecting && origin.protocol !== "https:") connected();
          else socket.once(origin.protocol === "https:" ? "secureConnect" : "connect", connected);
          if (attemptSignal.aborted) socket.destroy();
        });
        outgoing.setHeader("authorization", "Bearer " + token);
        outgoing.end(request.body);
      });
    },
    async close() {
      activeSignal?.removeEventListener("abort", destroy);
      destroy();
      // Join actual close events before a retry or completed public cleanup.
      await requestClosed;
      await responseClosed;
      await socketClosed;
    },
  });
}

module.exports = { runAttempt, cancelled, timeout };
