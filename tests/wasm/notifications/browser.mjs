//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { HttpEngine, HttpNotificationSubscription, NotificationError } from "../../../crates/uqa-wasm/js/index.mjs";

const options = { maxChannels: 2, maxQueuedEvents: 16, maxQueuedBytes: 65536,
  maxTransportChunkBytes: 65536, connectTimeoutMs: 3000, readyTimeoutMs: 5000, maxIdleTimeoutMs: 5000 };
const retry = { maxAttempts: 2, episodeTimeoutMs: 4000, initialBackoffMs: 1, maxBackoffMs: 2, maxRetryAfterMs: 1000 };
const report = { schema_version: 1, status: "Running", passed: [], error: null };
const render = () => { document.querySelector("#report").textContent = JSON.stringify(report); };
const equal = (actual, expected) => { if (actual !== expected) throw new Error(`Expected ${String(expected)}, got ${String(actual)}`); };
const assert = (value, message) => { if (!value) throw new Error(message); };
const wire = (kind, value) => "event: " + kind + "\ndata: " + JSON.stringify(value) + "\n\n";
const data = (text) => JSON.parse(text.split("data: ")[1]);
async function control(command) {
  const response = await fetch("/__notification_control", { method: "POST", body: JSON.stringify(command) });
  return response.json();
}
async function setup(mode) {
  const { origin, fixture } = await control({ action: "configure", mode });
  return { engine: new HttpEngine(origin, "browser-fixture"), fixture };
}
async function rejects(promise, code) {
  let error;
  try { await promise; } catch (caught) { error = caught; }
  assert(error instanceof NotificationError, "expected a typed notification failure");
  equal(error.code, "NOTIFICATION_" + code);
  return error;
}
async function run(name, body) { await body(); report.passed.push(name); render(); }
async function main() {
  await run("bearer preflight, exact values, cookie/referrer omission and joined iterator return", async () => {
    const { engine, fixture } = await setup("normal");
    document.cookie = "request_cookie=forbidden; Path=/";
    const sub = await engine.subscribeNotifications(fixture.channels, options);
    try {
      assert(sub instanceof HttpNotificationSubscription, "public subscription export");
      const event = await sub.nextEvent();
      equal(event.sequence, 1n); equal(event.processId, -2147483648);
      equal(event.channel, "작업"); equal(event.payload, fixture.expected_payload);
      equal(event.epoch, fixture.stream_id); equal(event.requestId, fixture.request_id);
      assert(Object.isFrozen(event), "immutable event"); equal(JSON.stringify(event), "{}");
      const stats = await control({ action: "stats" });
      equal(stats.requests.length, 1); equal(stats.requests[0].body, fixture.valid_request);
      equal(stats.requests[0].path, "/v1/notifications/subscribe");
      equal(stats.requests[0].authorizationOK, true); equal(stats.requests[0].cookie, null); equal(stats.requests[0].referrer, null);
      assert(stats.preflights.some((item) => item.method === "POST" && item.headers.includes("authorization") && item.headers.includes("content-type")), "real bearer preflight");
      assert(!document.cookie.includes("response_cookie"), "response cookies must be omitted");
      const pending = sub.next(); await sub.return(); equal((await pending).done, true);
      equal((await control({ action: "wait", closed: 1 })).active, 0);
    } finally { await sub.close(); document.cookie = "request_cookie=; Max-Age=0; Path=/"; }
  });
  await run("readiness is gated by the complete ready frame", async () => {
    const { engine, fixture } = await setup("ready-pending");
    let resolved = false;
    const pending = engine.subscribeNotifications(fixture.channels, options).then((value) => { resolved = true; return value; });
    await control({ action: "wait", requests: 1 }); equal(resolved, false);
    await control({ action: "ready" });
    const sub = await pending; equal((await sub.nextEvent()).sequence, 1n);
    await sub.close(); await control({ action: "wait", closed: 1 });
  });
  for (const mode of ["headers-pending", "ready-pending"]) {
    await run("AbortSignal joins " + mode, async () => {
      const { engine, fixture } = await setup(mode); const signal = new AbortController();
      const pending = rejects(engine.subscribeNotifications(fixture.channels, { ...options, retry, signal: signal.signal }), "CANCELLED");
      await control({ action: "wait", requests: 1 }); signal.abort(); await pending;
      equal((await control({ action: "wait", closed: 1 })).requests.length, 1);
    });
  }
  await run("pending receipt cancellation retains one consumer and terminal failure", async () => {
    const { engine, fixture } = await setup("normal"); const signal = new AbortController();
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, signal: signal.signal });
    equal((await sub.nextEvent()).sequence, 1n);
    const pending = rejects(sub.nextEvent(), "CANCELLED");
    await rejects(sub.nextEvent(), "INVALID_REQUEST");
    signal.abort(); equal(sub.isClosed, true);
    await pending; await rejects(sub.nextEvent(), "CANCELLED");
    await sub.close(); equal(await sub.nextEvent(), null); await control({ action: "wait", closed: 1 });
  });
  for (const [mode, code] of [["retry-pending", "SOURCE_UNAVAILABLE"], ["hidden-identity", "PROTOCOL"], ["gzip", "PROTOCOL"], ["malformed", "PROTOCOL"], ["redirect", "PROTOCOL"], ["unsupported", "UNSUPPORTED"]]) {
    await run("typed rejection: " + mode, async () => {
      const { engine, fixture } = await setup(mode);
      await rejects(engine.subscribeNotifications(fixture.channels, options), code);
      const stats = await control({ action: "wait", closed: 1 }); equal(stats.redirects, 0); equal(stats.requests.length, 1);
    });
  }
  await run("gap and new identity precede replacement data", async () => {
    const { engine, fixture } = await setup("reconnect");
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, retry });
    try {
      const first = await sub.nextEvent(); await control({ action: "drop" });
      const gap = await sub.nextEvent(); equal(gap.kind, "resync_required"); equal(gap.epoch, first.epoch);
      const ready = await sub.nextEvent(); equal(ready.kind, "reconnected"); assert(ready.epoch !== first.epoch, "replacement identity");
      const next = await sub.nextEvent(); equal(next.epoch, ready.epoch); equal(next.sequence, 1n); equal(sub.epoch, ready.epoch);
    } finally {
      await sub.close();
      const stats = await control({ action: "stats" });
      await control({ action: "wait", closed: stats.requests.length });
    }
  });
  await run("cancellation interrupts real Retry-After backoff without another request", async () => {
    const { engine, fixture } = await setup("backoff"); const signal = new AbortController();
    const sub = await engine.subscribeNotifications(fixture.channels, { ...options, retry, signal: signal.signal });
    const originalRandom = crypto.getRandomValues;
    let samples = 0, sampled;
    const inBackoff = new Promise((resolve) => { sampled = resolve; });
    // Observe real random sampling, keeping its values unchanged. The second
    // sample follows the complete replacement error and precedes its backoff.
    crypto.getRandomValues = function (value) {
      const result = originalRandom.call(this, value);
      if (++samples === 2) queueMicrotask(sampled);
      return result;
    };
    try {
      equal((await sub.nextEvent()).sequence, 1n);
      await control({ action: "drop" }); equal((await sub.nextEvent()).kind, "resync_required");
      const pending = rejects(sub.nextEvent(), "CANCELLED");
      await inBackoff; signal.abort(); await pending;
      const stats = await control({ action: "wait", closed: 2 }); equal(stats.requests.length, 2);
    } finally {
      crypto.getRandomValues = originalRandom;
      await sub.close();
      const stats = await control({ action: "stats" });
      await control({ action: "wait", closed: stats.requests.length });
    }
  });
  await run("one overflowing consumer leaves the healthy iterator usable", async () => {
    const { engine, fixture } = await setup("broadcast");
    const slow = await engine.subscribeNotifications(fixture.channels, { ...options, maxQueuedEvents: 1 });
    const healthy = await engine.subscribeNotifications(fixture.channels, options);
    try {
      const next = data(fixture.notification); next.sequence = "2";
      await control({ action: "broadcast", wire: fixture.notification + wire("notification", next) });
      await control({ action: "wait", closed: 1 });
      equal((await slow.nextEvent()).sequence, 1n);
      await rejects(slow.nextEvent(), "BACKPRESSURE");
      equal((await healthy.nextEvent()).sequence, 1n); equal((await healthy.nextEvent()).sequence, 2n);
    } finally { await Promise.all([slow.close(), healthy.close()]); await control({ action: "wait", closed: 2 }); }
  });
  await run("observed authorization loss discards queued notifications", async () => {
    const { engine, fixture } = await setup("broadcast");
    const sub = await engine.subscribeNotifications(fixture.channels, options);
    const error = { ...data(fixture.error), code: "NOTIFICATION_AUTHORITY_REVOKED", retryable: false };
    await control({ action: "broadcast", wire: fixture.notification + wire("error", error) });
    await control({ action: "wait", closed: 1 }); await rejects(sub.nextEvent(), "AUTHORITY_REVOKED"); await sub.close();
  });
  await run("silent stream produces the shared idle timeout", async () => {
    const { engine, fixture } = await setup("idle"); const sub = await engine.subscribeNotifications(fixture.channels, options);
    const error = await rejects(sub.nextEvent(), "TIMEOUT"); equal(error.timeoutStage, "idle");
    await sub.close(); await control({ action: "wait", closed: 1 });
  });
  report.status = "Passed";
}
render();
main().catch((error) => { report.status = "Failed"; report.error = error.stack ?? String(error); }).finally(render);
