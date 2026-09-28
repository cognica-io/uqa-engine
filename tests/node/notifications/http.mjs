//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { createServer as createTCPServer } from "node:net";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { inspect } from "node:util";

const fixture = JSON.parse(readFileSync(new URL("../../../crates/uqa-client/tests/fixtures/notifications-v1.json", import.meta.url)));
const options = { maxChannels: 2, maxQueuedEvents: 16, maxQueuedBytes: 65536,
  maxTransportChunkBytes: 65536, connectTimeoutMs: 2000, readyTimeoutMs: 3000, maxIdleTimeoutMs: 5000 };
const retry = { maxAttempts: 2, episodeTimeoutMs: 3000, initialBackoffMs: 1, maxBackoffMs: 2, maxRetryAfterMs: 1000 };
const deferred = () => { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; };
const turn = () => new Promise((resolve) => setImmediate(resolve));
const data = (wire) => JSON.parse(wire.split("data: ")[1]);
const wire = (kind, value) => "event: " + kind + "\ndata: " + JSON.stringify(value) + "\n\n";
const code = (expected) => (error) => error.code === "NOTIFICATION_" + expected;
function headers(response, overrides = {}) {
  response.writeHead(200, { "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-store, no-transform", "x-request-id": fixture.request_id, ...overrides });
}
async function server(t, handler, tcp = false) {
  const errors = [];
  const instance = (tcp ? createTCPServer : createServer)((...args) => {
    Promise.resolve().then(() => handler(...args)).catch((error) => { errors.push(error); args.at(-1).destroy(); });
  });
  const sockets = new Set();
  instance.on("connection", (socket) => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  await new Promise((resolve) => instance.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => instance.close(resolve));
    assert.deepEqual(errors, []);
  });
  return { url: "http://127.0.0.1:" + instance.address().port, sockets };
}
async function body(request) {
  const bytes = [];
  for await (const chunk of request) bytes.push(chunk);
  return Buffer.concat(bytes).toString();
}
function jsonError(response, status, errorCode, extra = {}, raw) {
  response.writeHead(status, { "content-type": "application/json", "x-request-id": fixture.request_id, ...extra });
  response.end(raw ?? JSON.stringify({ error: { code: errorCode, message: "private diagnostic" }, request_id: fixture.request_id }));
}

export function registerNotificationHTTPTests(packagePath) {
  const require = createRequire(join(packagePath, "package.json"));
  const { HttpEngine, NotificationError, HttpNotificationSubscription } = require("./api.js");
  const { NotificationEvent } = require("./notification-error.js");
  const { NotificationQueue } = require("./notification-queue.js");
  const { retryAfter } = require("./notification-headers.js");

  test("notification queue preserves full-width values and accounts bytes, slots and independent overflow", () => {
    const value = new NotificationEvent({ kind: "notification", epoch: fixture.stream_id,
      requestId: fixture.request_id, sequence: (1n << 64n) - 1n, processId: -2147483648,
      channel: "작업", payload: fixture.expected_payload });
    const cost = 33 + fixture.request_id.length + Buffer.byteLength(value.channel) + Buffer.byteLength(value.payload);
    const queue = new NotificationQueue(2, 16 + cost);
    queue.push(value);
    assert.equal(queue.chargedBytes, 16 + cost);
    assert.throws(() => queue.push(value), code("BACKPRESSURE"));
    const received = queue.take();
    for (const key of ["kind", "epoch", "requestId", "sequence", "processId", "channel", "payload", "cause"]) assert.equal(received[key], value[key]);
    assert.equal(queue.chargedBytes, 16);
    for (let i = 0; i < 4; i += 1) { queue.push(value); assert.equal(queue.take().sequence, value.sequence); }
    queue.clear();
    assert.equal(queue.length, 0);
    assert.equal(queue.chargedBytes, 0);
    assert.throws(() => new NotificationQueue(3, 23), code("CAPACITY"));
    const count = new NotificationQueue(1, 65536);
    count.push(value);
    assert.throws(() => count.push(value), code("BACKPRESSURE"));
  });

  test("notification Retry-After validates seconds and all three strict HTTP-date forms", () => {
    const at = Date.UTC(1994, 10, 6, 8, 49, 36);
    const response = (raw) => ({ rawHeaders: ["Retry-After", raw] });
    for (const raw of ["001", "Sun, 06 Nov 1994 08:49:37 GMT", "Sunday, 06-Nov-94 08:49:37 GMT", "Sun Nov  6 08:49:37 1994"]) {
      assert.equal(retryAfter(response(raw), { retry }, at), 1000);
    }
    for (const raw of ["-1", "1.1", "1994-11-06", "Mon, 06 Nov 1994 08:49:37 GMT", "Sun, 31 Feb 1994 08:49:37 GMT", "2", "9999999999999999999999"]) {
      assert.throws(() => retryAfter(response(raw), { retry }, at), code("PROTOCOL"));
    }
    assert.equal(retryAfter(response("0"), { retry }, at), 0);
    assert.throws(() => retryAfter({ rawHeaders: ["Retry-After", "0", "retry-after", "0"] }, { retry }, at), code("PROTOCOL"));
  });

  test("HTTP notification registration waits for actual ready and retains the exact original request", { timeout: 15000 }, async (t) => {
    const head = deferred(); const release = deferred(); const closed = deferred();
    const peer = await server(t, async (request, response) => {
      assert.equal(request.url, "/v1/notifications/subscribe");
      assert.equal(request.headers.authorization, "Bearer secret-token");
      assert.equal(request.headers["accept-encoding"], "identity");
      assert.equal(request.headers.accept, "text/event-stream");
      assert.equal(await body(request), fixture.valid_request);
      response.once("close", closed.resolve);
      headers(response); response.flushHeaders(); head.resolve();
      await release.promise;
      for (const byte of Buffer.from(fixture.ready + fixture.notification)) { response.write(Buffer.from([byte])); await turn(); }
    });
    const input = [...fixture.channels]; let ready = false;
    const pending = new HttpEngine(peer.url, "secret-token").subscribeNotifications(input, options).then((value) => { ready = true; return value; });
    input[0] = "modified";
    await head.promise; await turn(); assert.equal(ready, false); release.resolve();
    const sub = await pending;
    assert.ok(sub instanceof HttpNotificationSubscription);
    assert.equal(sub.epoch, fixture.stream_id);
    const event = await sub.nextEvent();
    assert.equal(event.sequence, 1n); assert.equal(event.processId, -2147483648); assert.equal(event.payload, fixture.expected_payload);
    assert.equal(inspect(event).includes(fixture.expected_payload), false);
    const waiting = sub.next();
    await assert.rejects(sub.next(), code("INVALID_REQUEST"));
    await Promise.all([sub.close(), sub.close()]);
    assert.deepEqual(await waiting, { done: true, value: undefined });
    await closed.promise;
    assert.equal(sub.isClosed, true);
    assert.deepEqual(await sub.next(), { done: true, value: undefined });
  });

  test("HTTP notifications abort actual pending readiness and idle receive, and return closes iteration", { timeout: 15000 }, async (t) => {
    let calls = 0; const accepted = deferred(); const socketsClosed = [];
    const peer = await server(t, (request, response) => {
      calls += 1; const closed = deferred(); socketsClosed.push(closed.promise); response.once("close", closed.resolve);
      headers(response); response.flushHeaders();
      if (calls > 1) response.write(fixture.ready);
      accepted.resolve();
    });
    const engine = new HttpEngine(peer.url, "secret");
    const early = new AbortController(); early.abort();
    await assert.rejects(engine.subscribeNotifications(fixture.channels, { ...options, signal: early.signal }), code("CANCELLED"));
    assert.equal(calls, 0);
    const registering = new AbortController();
    const pending = engine.subscribeNotifications(fixture.channels, { ...options, signal: registering.signal });
    await accepted.promise; registering.abort();
    await assert.rejects(pending, code("CANCELLED"));
    await socketsClosed[0];
    const controller = new AbortController();
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, signal: controller.signal });
    const waiting = sub.next(); controller.abort();
    await assert.rejects(waiting, code("CANCELLED"));
    await socketsClosed[1]; await sub.close();
    const iter = await engine.subscribeNotifications(fixture.channels, options);
    await iter.return(); await socketsClosed[2];
    assert.equal(iter.isClosed, true);
  });

  test("HTTP notification replacement exposes gap, then new identity before replacement values", { timeout: 15000 }, async (t) => {
    let calls = 0; const id = "request_2"; const epoch = "9fb52b7f-bdca-4db2-9ee0-490f99857202";
    const peer = await server(t, async (request, response) => {
      calls += 1;
      assert.equal(await body(request), fixture.valid_request);
      if (calls === 1) { headers(response); response.end(fixture.ready + fixture.notification + fixture.closed); }
      else {
        headers(response, { "x-request-id": id });
        response.write(wire("ready", { ...data(fixture.ready), request_id: id, stream_id: epoch }) +
          wire("notification", { ...data(fixture.notification), request_id: id, stream_id: epoch }));
      }
    });
    const sub = await new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, { ...options, retry });
    try {
      assert.equal((await sub.nextEvent()).sequence, 1n);
      const gap = await sub.nextEvent(); assert.equal(gap.kind, "resync_required"); assert.equal(gap.cause, "NOTIFICATION_SERVER_DRAINING");
      assert.equal(sub.epoch, fixture.stream_id);
      const connected = await sub.nextEvent(); assert.equal(connected.kind, "reconnected"); assert.equal(connected.requestId, id);
      assert.equal(sub.epoch, epoch); assert.equal(sub.requestId, id);
      const event = await sub.nextEvent(); assert.equal(event.epoch, epoch); assert.equal(event.sequence, 1n);
      assert.equal(calls, 2);
    } finally { await sub.close(); }
  });

  test("HTTP notification retry exhaustion retains original and last causes with exact attempt count", { timeout: 15000 }, async (t) => {
    let calls = 0;
    const peer = await server(t, (request, response) => {
      calls += 1;
      if (calls === 1) { headers(response); response.end(fixture.ready + fixture.closed); }
      else jsonError(response, 503, "NOTIFICATION_SOURCE_UNAVAILABLE", { "retry-after": "0" });
    });
    const sub = await new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, { ...options, retry });
    assert.equal((await sub.nextEvent()).kind, "resync_required");
    await assert.rejects(sub.nextEvent(), (error) => {
      assert.ok(error instanceof NotificationError);
      assert.equal(error.originalFailure.code, "NOTIFICATION_SERVER_DRAINING");
      assert.equal(error.lastAttemptFailure.code, "NOTIFICATION_SOURCE_UNAVAILABLE");
      assert.equal(error.lastAttemptFailure.httpStatus, 503);
      assert.equal(error.reconnectAttempts, 2);
      assert.equal(error.lastAttemptFailure.diagnostic.message, "private diagnostic");
      assert.equal(inspect(error).includes("private diagnostic"), false);
      assert.equal(JSON.stringify(error).includes("private diagnostic"), false);
      return true;
    });
    assert.equal(calls, 3); await sub.close();
  });

  test("HTTP notification queue overflow preserves its admitted prefix and authority loss discards unsent values", { timeout: 15000 }, async (t) => {
    let authority = false;
    const peer = await server(t, (request, response) => {
      headers(response);
      response.end(fixture.ready + fixture.notification + (authority
        ? wire("error", { ...data(fixture.error), code: "NOTIFICATION_AUTHORITY_REVOKED", retryable: true })
        : wire("notification", { ...data(fixture.notification), sequence: "2" })));
    });
    const engine = new HttpEngine(peer.url, "secret");
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, maxQueuedEvents: 1 });
    while (!sub.isClosed) await turn();
    assert.equal((await sub.nextEvent()).sequence, 1n);
    await assert.rejects(sub.nextEvent(), code("BACKPRESSURE")); await sub.close();
    authority = true;
    const revoked = await engine.subscribeNotifications(fixture.channels, options);
    while (!revoked.isClosed) await turn();
    await assert.rejects(revoked.nextEvent(), code("AUTHORITY_REVOKED")); await revoked.close();
  });

  test("HTTP notifications reject corrupt response headers, redirects and invalid bounded error envelopes", { timeout: 15000 }, async (t) => {
    let serve;
    const peer = await server(t, (request, response) => serve(response));
    const engine = new HttpEngine(peer.url, "secret");
    for (const fields of [{ "content-type": "text/event-stream" }, { "content-encoding": "gzip" },
      { "cache-control": "no-store" }, { "x-request-id": [fixture.request_id, fixture.request_id] }]) {
      serve = (response) => { headers(response, fields); response.end(fixture.ready); };
      await assert.rejects(engine.subscribeNotifications(fixture.channels, options), code("PROTOCOL"));
    }
    for (const status of [404, 405, 501]) {
      serve = (response) => { response.writeHead(status); response.end("unsupported"); };
      await assert.rejects(engine.subscribeNotifications(fixture.channels, options), code("UNSUPPORTED"));
    }
    serve = (response) => { response.writeHead(307, { location: peer.url + "/other" }); response.end(); };
    await assert.rejects(engine.subscribeNotifications(fixture.channels, options), code("PROTOCOL"));
    for (const raw of ['{"request_id":"request_1","error":{"code":"X","message":"a","message":"b"}}',
      '{"request_id":"other","error":{"code":"X","message":"a"}}', "x".repeat(65537)]) {
      serve = (response) => jsonError(response, 503, "X", {}, raw);
      await assert.rejects(engine.subscribeNotifications(fixture.channels, options), code("PROTOCOL"));
    }
    serve = (response) => jsonError(response, 401, "TOKEN_INVALID");
    await assert.rejects(engine.subscribeNotifications(fixture.channels, { ...options, retry }), code("AUTHENTICATION"));
  });

  test("HTTP notification readiness deadline is not extended by comments and idle silence stays typed", { timeout: 15000 }, async (t) => {
    let phase = 0;
    const peer = await server(t, (request, response) => {
      headers(response);
      if (phase > 0) response.write(fixture.ready);
      if (phase === 0 || phase === 2) {
        const heartbeat = setInterval(() => response.write(": heartbeat\n\n"), 10);
        response.once("close", () => clearInterval(heartbeat));
      }
    });
    const engine = new HttpEngine(peer.url, "secret");
    await assert.rejects(engine.subscribeNotifications(fixture.channels, { ...options, connectTimeoutMs: 500, readyTimeoutMs: 500 }),
      (error) => code("TIMEOUT")(error) && error.timeoutStage === "readiness");
    phase = 1;
    const idle = await engine.subscribeNotifications(fixture.channels, options);
    await assert.rejects(idle.nextEvent(), (error) => code("TIMEOUT")(error) && error.timeoutStage === "idle");
    await idle.close();
    phase = 2;
    const healthy = await engine.subscribeNotifications(fixture.channels, { ...options, connectTimeoutMs: 200, readyTimeoutMs: 200 });
    await new Promise((resolve) => setTimeout(resolve, 550));
    assert.equal(healthy.isClosed, false); await healthy.close();
  });

  test("HTTP notification TLS connection cancellation and malformed HTTP do not leak sockets", { timeout: 15000 }, async (t) => {
    const connected = deferred(); const closed = deferred();
    const peer = await server(t, (socket) => { socket.once("close", closed.resolve); socket.on("data", () => connected.resolve()); }, true);
    const controller = new AbortController();
    const pending = new HttpEngine(peer.url.replace("http:", "https:"), "secret").subscribeNotifications(fixture.channels, { ...options, signal: controller.signal });
    await connected.promise; controller.abort(); await assert.rejects(pending, code("CANCELLED")); await closed.promise;
    const corrupt = await server(t, (socket) => { socket.once("data", () => socket.end("HTTP/broken response\r\n\r\n")); }, true);
    await assert.rejects(new HttpEngine(corrupt.url, "secret").subscribeNotifications(fixture.channels, options), code("PROTOCOL"));
  });

  test("HTTP notification cancellation interrupts backoff without issuing another request", { timeout: 15000 }, async (t) => {
    let calls = 0;
    const peer = await server(t, (request, response) => { calls += 1; headers(response); response.end(fixture.ready + fixture.closed); });
    const controller = new AbortController();
    const sub = await new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, {
      ...options, signal: controller.signal, retry: { ...retry, episodeTimeoutMs: 20000, initialBackoffMs: 10000, maxBackoffMs: 10000 },
    });
    assert.equal((await sub.nextEvent()).kind, "resync_required");
    const pending = sub.nextEvent(); controller.abort();
    await assert.rejects(pending, code("CANCELLED")); await sub.close(); assert.equal(calls, 1);
  });

  test("HTTP notification corrupt terminal suffix and reused replacement epoch stay nonretryable", { timeout: 15000 }, async (t) => {
    let calls = 0; let suffix = true;
    const peer = await server(t, (request, response) => {
      calls += 1; headers(response); response.end(fixture.ready + fixture.closed + (suffix ? "event: ready\n\n" : ""));
    });
    const engine = new HttpEngine(peer.url, "secret");
    const corrupt = await engine.subscribeNotifications(fixture.channels, { ...options, retry });
    await assert.rejects(corrupt.nextEvent(), code("PROTOCOL")); await corrupt.close(); assert.equal(calls, 1);
    calls = 0; suffix = false;
    const replaced = await engine.subscribeNotifications(fixture.channels, { ...options, retry });
    assert.equal((await replaced.nextEvent()).kind, "resync_required");
    await assert.rejects(replaced.nextEvent(), (error) => code("PROTOCOL")(error) && error.reconnectAttempts === 1);
    await replaced.close(); assert.equal(calls, 2);
  });

  test("HTTP notification server guidance cannot extend the reconnect episode and initial failures are not retried", { timeout: 15000 }, async (t) => {
    let calls = 0; let initialFailure = false;
    const peer = await server(t, (request, response) => {
      calls += 1;
      if (calls === 1 && !initialFailure) { headers(response); response.end(fixture.ready + fixture.closed); }
      else jsonError(response, 503, "NOTIFICATION_SOURCE_UNAVAILABLE", { "retry-after": "2" });
    });
    const engine = new HttpEngine(peer.url, "secret");
    const policy = { ...retry, episodeTimeoutMs: 1000, maxRetryAfterMs: 2500 };
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, retry: policy });
    assert.equal((await sub.nextEvent()).kind, "resync_required");
    await assert.rejects(sub.nextEvent(), (error) => code("TIMEOUT")(error) && error.lastAttemptFailure.timeoutStage === "reconnect" && error.reconnectAttempts === 1);
    await sub.close(); assert.equal(calls, 2);
    calls = 0; initialFailure = true;
    await assert.rejects(engine.subscribeNotifications(fixture.channels, { ...options, retry: policy }), code("SOURCE_UNAVAILABLE"));
    assert.equal(calls, 1);
  });

  test("HTTP notification cumulative input exceeds a frame limit while a consumer keeps exact FIFO order", { timeout: 15000 }, async (t) => {
    const count = 400;
    const peer = await server(t, (request, response) => {
      headers(response); response.write(fixture.ready);
      for (let n = 1; n <= count; n += 1) response.write(wire("notification", { ...data(fixture.notification), sequence: String(n) }));
    });
    const sub = await new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, options);
    try {
      for (let n = 1; n <= count; n += 1) assert.equal((await sub.nextEvent()).sequence, BigInt(n));
    } finally { await sub.close(); }
  });

  test("HTTP notification header validation includes duplicates beyond Node's default header count", { timeout: 15000 }, async (t) => {
    const peer = await server(t, (socket) => {
      socket.once("data", () => socket.end("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\n" +
        "Cache-Control: no-store, no-transform\r\nX-Request-ID: request_1\r\n" + "X: v\r\n".repeat(2100) +
        "X-Request-ID: hidden_duplicate\r\nContent-Length: " + Buffer.byteLength(fixture.ready) + "\r\nConnection: close\r\n\r\n" + fixture.ready));
    }, true);
    await assert.rejects(async () => {
      const sub = await new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, options);
      await sub.close();
    }, code("PROTOCOL"));
  });

  test("HTTP notification upgrade responses are protocol failures with completed socket cleanup", { timeout: 15000 }, async (t) => {
    const closed = deferred();
    const peer = await server(t, (socket) => {
      socket.once("close", closed.resolve);
      socket.once("data", () => socket.write("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n"));
    }, true);
    await assert.rejects(new HttpEngine(peer.url, "secret").subscribeNotifications(fixture.channels, options), code("PROTOCOL"));
    await closed.promise;
  });
}
