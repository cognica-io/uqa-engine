//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { createContext, runInContext } from "node:vm";

const fixture = JSON.parse(readFileSync(new URL("../../../crates/uqa-client/tests/fixtures/notifications-v1.json", import.meta.url), "utf8"));

function webCore(packagePath) {
  const context = createContext({ TextEncoder, TextDecoder, AbortController, setTimeout, clearTimeout });
  const modules = new Map();
  const load = (name) => {
    assert.match(name, /^\.\/[a-z-]+\.js$/, "shared notification code must not import Node builtins");
    if (modules.has(name)) return modules.get(name).exports;
    const module = { exports: {} };
    modules.set(name, module);
    const source = readFileSync(join(packagePath, name), "utf8");
    runInContext("(function(require, module) {\n" + source + "\n})", context, { filename: name })(load, module);
    return module.exports;
  };
  assert.equal(runInContext("typeof Buffer", context), "undefined");
  return load;
}

export function registerNotificationPortabilityTests(packagePath) {
  test("notification abort discards queued values and joins owned cleanup", async (t) => {
    const load = webCore(packagePath);
    const { subscribe } = load("./notification-subscription.js");
    const { NotificationEvent, cancelled } = load("./notification-error.js");
    const controller = new AbortController();
    let release, started, subscription;
    const cleanup = new Promise((resolve) => { release = resolve; });
    const cleaning = new Promise((resolve) => { started = resolve; });
    t.after(async () => { release(); await subscription?.close(); });
    const runtime = { now: () => performance.now(), randomInt: (lower) => lower,
      async runAttempt(connection, signal, deadline, onReady, onEvent) {
        const identity = { epoch: fixture.stream_id, requestId: fixture.request_id };
        onReady(identity);
        onEvent(new NotificationEvent({ kind: "notification", ...identity, sequence: 1n,
          processId: 1, channel: "jobs", payload: "queued before cancellation" }));
        await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
        started();
        await cleanup;
        throw cancelled();
      } };
    subscription = await subscribe(new URL("http://127.0.0.1/"), "test-only", fixture.channels, {
      signal: controller.signal, maxChannels: 2, maxQueuedEvents: 2, maxQueuedBytes: 65536,
      maxTransportChunkBytes: 65536, connectTimeoutMs: 1000, readyTimeoutMs: 1000, maxIdleTimeoutMs: 1000,
    }, runtime);
    controller.abort();
    const receiving = subscription.nextEvent();
    let settled = false;
    receiving.then(() => { settled = true; }, () => { settled = true; });
    await cleaning;
    await Promise.resolve();
    assert.equal(settled, false, "cancelled receive must join retained cleanup instead of yielding queued data");
    assert.equal(subscription.isClosed, true, "cancellation ends delivery before cleanup completes");
    release();
    await assert.rejects(receiving, { code: "NOTIFICATION_CANCELLED" });
    await assert.rejects(subscription.nextEvent(), { code: "NOTIFICATION_CANCELLED" });
    await subscription.close();
    assert.equal(await subscription.nextEvent(), null);
  });

  test("notification header and error parsing accepts web byte streams without Node globals", async () => {
    const load = webCore(packagePath);
    const { streamHeaders, responseError } = load("./notification-headers.js");
    const head = ["Content-Type", "text/event-stream; charset=utf-8", "Cache-Control", "no-store, no-transform",
      "X-Request-Id", fixture.request_id];
    assert.equal(streamHeaders({ rawHeaders: head }), fixture.request_id);
    assert.throws(() => streamHeaders({ rawHeaders: [...head, "Content-Encoding", "gzip"] }),
      (error) => error.code === "NOTIFICATION_PROTOCOL");
    const body = new TextEncoder().encode(JSON.stringify({ error: { code: "AUTHENTICATION", message: "비공개" }, request_id: fixture.request_id }));
    const response = { statusCode: 401, rawHeaders: ["Content-Type", "application/json", "X-Request-Id", fixture.request_id],
      async *[Symbol.asyncIterator]() { for (const byte of body) yield Uint8Array.of(byte); } };
    const error = await responseError(response, { retry: null, maxTransportChunkBytes: 1 }, () => {});
    assert.equal(error.code, "NOTIFICATION_AUTHENTICATION");
    assert.equal(error.requestId, fixture.request_id);
    assert.equal(error.diagnostic.message, "비공개");
    assert.equal(String(error).includes("비공개"), false);
  });

  test("notification lifecycle without Node globals preserves gaps and joins cancelled runtime work", { timeout: 15000 }, async (t) => {
    const load = webCore(packagePath);
    const { subscribe } = load("./notification-subscription.js");
    const { NotificationError, NotificationEvent, cancelled } = load("./notification-error.js");
    const options = { maxChannels: 2, maxQueuedEvents: 8, maxQueuedBytes: 65536,
      maxTransportChunkBytes: 65536, connectTimeoutMs: 2000, readyTimeoutMs: 3000, maxIdleTimeoutMs: 5000,
      retry: { maxAttempts: 1, episodeTimeoutMs: 3000, initialBackoffMs: 1, maxBackoffMs: 1, maxRetryAfterMs: 1000 } };
    const replacement = "3bf442cb-1a7b-45e4-a571-198c04af15e9";
    let releaseCleanup, startedCleanup, sub;
    const cleanup = new Promise((resolve) => { releaseCleanup = resolve; });
    const cancelling = new Promise((resolve) => { startedCleanup = resolve; });
    t.after(async () => { releaseCleanup(); await sub?.close(); });
    let attempts = 0;
    const runtime = { now: () => performance.now(), randomInt: (lower) => lower,
      async runAttempt(connection, signal, deadline, onReady, onEvent) {
        attempts += 1;
        assert.ok(attempts <= 2);
        assert.equal(connection.request.body, fixture.valid_request);
        const epoch = attempts === 1 ? fixture.stream_id : replacement;
        onReady({ epoch, requestId: fixture.request_id });
        onEvent(new NotificationEvent({ kind: "notification", epoch, requestId: fixture.request_id,
          sequence: 1n, processId: -2147483648, channel: "작업", payload: fixture.expected_payload }));
        if (attempts === 1) throw new NotificationError("NOTIFICATION_TRANSPORT", { retryable: true });
        assert.ok(Number.isFinite(deadline));
        await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
        startedCleanup();
        await cleanup;
        throw cancelled();
      } };
    sub = await subscribe(new URL("http://127.0.0.1/"), "private", fixture.channels, options, runtime);
    const first = await sub.nextEvent();
    assert.equal(first.kind, "notification");
    assert.equal(first.sequence, 1n);
    assert.equal(first.payload, fixture.expected_payload);
    assert.equal((await sub.nextEvent()).kind, "resync_required");
    assert.equal(sub.epoch, fixture.stream_id);
    assert.equal((await sub.nextEvent()).kind, "reconnected");
    assert.equal(sub.epoch, replacement);
    assert.equal((await sub.nextEvent()).epoch, replacement);
    let closed = false;
    const closing = sub.close().then(() => { closed = true; });
    await cancelling;
    await Promise.resolve();
    assert.equal(closed, false);
    releaseCleanup();
    await closing;
    assert.equal(sub.isClosed, true);
    assert.equal(await sub.nextEvent(), null);
    assert.equal(attempts, 2);
  });

  test("notification core runs without Node globals across every wire split", () => {
    const load = webCore(packagePath);
    const { subscriptionRequest } = load("./notification-request.js");
    const { NotificationDecoder } = load("./notification-protocol.js");
    const request = subscriptionRequest(fixture.channels, 2);
    assert.equal(request.body, fixture.valid_request);
    const bytes = new TextEncoder().encode(fixture.ready + fixture.notification + fixture.closed);
    for (let split = 0; split <= bytes.length; split += 1) {
      const receiver = new NotificationDecoder(request, fixture.request_id, 10000);
      const events = [];
      for (const part of [bytes.subarray(0, split), bytes.subarray(split)]) {
        for (let offset = 0; offset < part.length;) {
          const step = receiver.decode(part.subarray(offset));
          assert.ok(step.consumed > 0 || step.event !== null);
          offset += step.consumed;
          if (step.event !== null) events.push(step.event);
        }
      }
      assert.deepEqual(events.map((event) => event.kind), ["ready", "notification", "closed"]);
      assert.equal(events[1].sequence, 1n);
      assert.equal(events[1].processId, -2147483648);
      assert.equal(events[1].channel, "작업");
      assert.equal(events[1].payload, fixture.expected_payload);
      assert.equal(receiver.finish(), null);
    }
  });

  test("notification web bytes preserve UTF-8 ordering, BOMs and exact byte counts", () => {
    const load = webCore(packagePath);
    const { subscriptionRequest } = load("./notification-request.js");
    const { byteLength, encodeInto, decode } = load("./notification-bytes.js");
    const channels = ["𐀀", "\ufefftopic", "\ue000", "é", "a"];
    assert.deepEqual(Array.from(subscriptionRequest(channels, 5).channels), ["a", "é", "\ue000", "\ufefftopic", "𐀀"]);
    for (const text of ["", "\ufeff", "a\0\n", "é한😀", "\ud800", "\udfff", "\ud800a", "\ud800\udfff"]) {
      const expected = Buffer.from(text);
      assert.equal(byteLength(text), expected.length);
      const destination = new Uint8Array(expected.length + 2).fill(0xff);
      assert.equal(encodeInto(text, destination.subarray(1, -1)), expected.length);
      assert.deepEqual(Array.from(destination.subarray(1, -1)), Array.from(expected));
      assert.equal(destination[0], 0xff);
      assert.equal(destination.at(-1), 0xff);
      assert.equal(decode(destination.subarray(1, -1)), expected.toString("utf8"));
    }
  });

  test("notification web queue preserves full-width values and its exact admission boundary", () => {
    const load = webCore(packagePath);
    const { NotificationQueue } = load("./notification-queue.js");
    const { NotificationEvent } = load("./notification-error.js");
    const value = { kind: "notification", epoch: fixture.stream_id, requestId: fixture.request_id,
      sequence: (1n << 64n) - 1n, processId: -2147483648, channel: "\ufeff작업", payload: "\ufeff😀\0\n" };
    const bytes = 18 + value.requestId.length + 15 + Buffer.byteLength(value.channel) + Buffer.byteLength(value.payload);
    const rejected = new NotificationQueue(1, 8 + bytes - 1);
    assert.throws(() => rejected.push(new NotificationEvent(value)), { code: "NOTIFICATION_BACKPRESSURE" });
    assert.equal(rejected.length, 0);
    assert.equal(rejected.chargedBytes, 8);
    const queue = new NotificationQueue(1, 8 + bytes);
    queue.push(new NotificationEvent(value));
    assert.equal(queue.chargedBytes, 8 + bytes);
    const result = queue.take();
    for (const [key, expected] of Object.entries(value)) assert.equal(result[key], expected);
    assert.equal(queue.chargedBytes, 8);
    for (const event of [
      { ...value, sequence: 1n, processId: 2147483647 },
      { kind: "resync_required", epoch: fixture.stream_id, requestId: null, cause: "NOTIFICATION_TRANSPORT" },
      { kind: "reconnected", epoch: fixture.stream_id, requestId: "again" },
    ]) {
      queue.push(new NotificationEvent(event));
      const actual = queue.take();
      for (const [key, expected] of Object.entries(event)) assert.equal(actual[key], expected);
    }
    queue.clear();
    assert.equal(queue.chargedBytes, 0);
  });
}
