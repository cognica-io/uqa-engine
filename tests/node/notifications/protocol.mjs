//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { inspect } from "node:util";

const fixture = JSON.parse(readFileSync(new URL("../../../crates/uqa-client/tests/fixtures/notifications-v1.json", import.meta.url), "utf8"));

export function registerNotificationProtocolTests(packagePath) {
  const require = createRequire(import.meta.url);
  const { subscriptionRequest, httpOptions, MAX_WIRE_BYTES } = require(join(packagePath, "notification-request.js"));
  const { NotificationDecoder, decimal } = require(join(packagePath, "notification-protocol.js"));
  const { NotificationError, NotificationEvent } = require(join(packagePath, "notification-error.js"));
  const { parseJSON } = require(join(packagePath, "http-json.js"));
  const request = () => subscriptionRequest(fixture.channels, 2);
  const decoder = (maximum = 10000) => new NotificationDecoder(request(), fixture.request_id, maximum);
  const data = (frame) => JSON.parse(frame.split("data: ")[1]);
  const wire = (kind, value) => "event: " + kind + "\ndata: " + JSON.stringify(value) + "\n\n";
  const protocolError = (error) => error instanceof NotificationError && error.code === "NOTIFICATION_PROTOCOL";
  function feed(receiver, input, events = []) {
    input = typeof input === "string" ? Buffer.from(input) : input;
    for (let offset = 0; offset < input.length;) {
      const step = receiver.decode(input.subarray(offset));
      assert.ok(step.consumed > 0 || step.event !== null, "decoder must make progress");
      offset += step.consumed;
      if (step.event !== null) events.push(step.event);
    }
    return events;
  }
  function notificationFailure(change) {
    const receiver = decoder();
    feed(receiver, fixture.ready);
    assert.throws(() => feed(receiver, wire("notification", { ...data(fixture.notification), ...change })), protocolError);
    assert.throws(() => feed(receiver, fixture.notification), protocolError);
  }

  test("notification requests bound exact UTF-8 channels and escaped bodies before copying", () => {
    const source = [...fixture.channels];
    const output = subscriptionRequest(source, 2);
    source[0] = "changed";
    assert.equal(output.body, fixture.valid_request);
    assert.deepEqual(output.channels, fixture.channels);
    for (const value of [[], "jobs", ["a", "a"], [""], ["a\0b"], ["a".repeat(64)],
      ["문".repeat(22)], ["\ud800"], ["\udfff"], [1], [null]]) {
      assert.throws(() => subscriptionRequest(value, 2), { code: "NOTIFICATION_INVALID_REQUEST" });
    }
    const many = Array(16385);
    Object.defineProperty(many, "0", { get() { assert.fail("oversized input must not be copied"); } });
    assert.throws(() => subscriptionRequest(many, 20000), { code: "NOTIFICATION_INVALID_REQUEST" });
    const escaped = Array.from({ length: 3000 }, (_, i) => String(i) + "\x01".repeat(40));
    assert.throws(() => subscriptionRequest(escaped, escaped.length), { code: "NOTIFICATION_INVALID_REQUEST" });
    assert.deepEqual(subscriptionRequest(["😀", "\ue000"], 2).channels, ["\ue000", "😀"]);
  });

  test("notification JSON validation rejects duplicates and containers beyond depth two without changing SQL defaults", () => {
    assert.equal(parseJSON('{"n":1,"n":2}').n, 2);
    assert.deepEqual(parseJSON(fixture.valid_request, { rejectDuplicates: true, maxContainerDepth: 2 }), {
      protocol_version: 1, channels: fixture.channels,
    });
    assert.throws(() => parseJSON(fixture.invalid_nested_request, { maxContainerDepth: 2 }));
    assert.throws(() => parseJSON('{"n":1,"\\u006e":2}', { rejectDuplicates: true }));
    assert.throws(() => parseJSON('{"payload":"\\ud800"}', { scalarUnicode: true }));
    assert.equal(parseJSON('"\\ud83d\\ude00"', { scalarUnicode: true }), "😀");
  });

  test("notification decoder preserves the independent literal fixture across every split", () => {
    const bytes = Buffer.from(fixture.ready + fixture.notification + fixture.closed);
    for (let split = 0; split <= bytes.length; split += 1) {
      const receiver = decoder();
      const events = feed(receiver, bytes.subarray(0, split));
      feed(receiver, bytes.subarray(split), events);
      assert.deepEqual(events.map((event) => event.kind), ["ready", "notification", "closed"]);
      const event = events[1];
      assert.equal(event.payload, fixture.expected_payload);
      assert.equal(event.channel, "작업");
      assert.equal(event.processId, -2147483648);
      assert.equal(event.sequence, 1n);
      assert.equal(event.epoch, fixture.stream_id);
      assert.equal(event.requestId, fixture.request_id);
      assert.equal(receiver.finish(), null);
    }
  });

  test("notification decoder accepts single-byte UTF-8, BOM, CRLF and multiline data", () => {
    const receiver = decoder();
    const ready = fixture.ready.replace("event: ready", "event: ignored\nevent: ready")
      .replace(',"request_id"', ',\ndata: "request_id"');
    const bytes = Buffer.from("\ufeff: before ready\n\n" + ready + fixture.notification + fixture.closed);
    const events = [];
    for (const byte of Buffer.from(bytes.toString().replaceAll("\n", "\r\n"))) {
      feed(receiver, Buffer.from([byte]), events);
    }
    assert.deepEqual(events.map((event) => event.kind), ["heartbeat", "ready", "notification", "closed"]);
    assert.equal(events[2].payload, fixture.expected_payload);
    assert.equal(receiver.finish(), null);
  });

  test("notification schemas reject unknown, duplicate, wrong-type and premature fields", () => {
    for (const input of [
      fixture.ready.replace('"protocol_version":1', '"protocol_version":1,"protocol_version":1'),
      fixture.ready.replace('"protocol_version":1', '"protocol_version":1,"\\u0070rotocol_version":1'),
      fixture.ready.replace('"protocol_version":1', '"protocol_version":1.0'),
      fixture.ready.replace('"protocol_version":1', '"protocol_version":2'),
      fixture.ready.replace('"delivery":"live"', '"delivery":"replay"'),
      fixture.ready.replace('"resume_supported":false', '"resume_supported":true'),
      fixture.ready.replace('"max_event_bytes":65536', '"max_event_bytes":65535'),
      fixture.ready.replace('"accepted_channel_count":2', '"accepted_channel_count":1'),
      "id: forbidden\n" + fixture.ready,
      "retry: 1\n" + fixture.ready,
      fixture.notification,
      fixture.ready.replace("event: ready", "event: unknown"),
      fixture.ready.replace('"delivery":"live"', '"delivery":{"nested":{"too":"deep"}}'),
    ]) {
      const receiver = decoder();
      assert.throws(() => feed(receiver, input), protocolError);
      assert.throws(() => feed(receiver, fixture.ready), protocolError);
    }
    const receiver = decoder();
    feed(receiver, fixture.ready);
    assert.throws(() => feed(receiver, fixture.ready), protocolError);
  });

  test("notification counters, sender bounds, channels and identities never coerce or round", () => {
    assert.equal(decimal("18446744073709551615"), 18446744073709551615n);
    for (const value of ["0", "01", "+1", "1.0", "1e0", "1\n", "18446744073709551616", 1]) {
      assert.throws(() => decimal(value), protocolError);
    }
    for (const change of [
      { sequence: "2" }, { sequence: "9007199254740993" }, { sequence: 1 },
      { process_id: 2147483648 }, { process_id: -2147483649 }, { process_id: "1" },
      { channel: "other" }, { payload: "\ud800" },
      { request_id: fixture.request_id + "other" }, { stream_id: fixture.stream_id.toUpperCase() },
    ]) notificationFailure(change);
    for (const token of ["-0", "1.0", "1e0"]) {
      const receiver = decoder();
      feed(receiver, fixture.ready);
      assert.throws(() => feed(receiver, fixture.notification.replace("-2147483648", token)), protocolError);
    }
    for (const id of ["", "x".repeat(129), "request_1\n", "private secret", "😀"]) {
      assert.throws(() => new NotificationDecoder(request(), id, 10000), protocolError);
    }
  });

  test("notification timing validates the strict relationship and actual JavaScript timer range", () => {
    for (const change of [
      { heartbeat_interval_ms: "0" }, { idle_timeout_ms: "400" }, { timing_margin_ms: "0" },
      { idle_timeout_ms: "2147483648" }, { heartbeat_interval_ms: "18446744073709551615" },
      { heartbeat_interval_ms: "100\n" },
    ]) assert.throws(() => feed(decoder(), wire("ready", { ...data(fixture.ready), ...change })), protocolError);
    assert.throws(() => feed(decoder(400), fixture.ready), protocolError);
    assert.equal(feed(decoder(401), fixture.ready)[0].idleTimeoutMs, 401);
    const options = { maxChannels: 2, maxQueuedEvents: 2, maxQueuedBytes: 1000,
      maxTransportChunkBytes: 65536, connectTimeoutMs: 100, readyTimeoutMs: 1000, maxIdleTimeoutMs: 5000 };
    assert.equal(httpOptions(options).retry, null);
    for (const change of [{ readyTimeoutMs: 1 }, { maxTransportChunkBytes: 65535 },
      { maxQueuedEvents: 0 }, { connectTimeoutMs: 2147483648 }, { maxChannels: 1.5 }]) {
      assert.throws(() => httpOptions({ ...options, ...change }), { code: "NOTIFICATION_INVALID_REQUEST" });
    }
  });

  test("notification frame limits include complete delimiters and deferred final CRLF", () => {
    const ending = fixture.ready.replaceAll("\n", "\r");
    const exact = Buffer.from(":" + "x".repeat(MAX_WIRE_BYTES - Buffer.byteLength(ending) - 2) + "\n" + ending);
    assert.equal(exact.length, MAX_WIRE_BYTES);
    const receiver = decoder();
    assert.equal(receiver.decode(exact).event, null);
    assert.equal(receiver.ready, null);
    assert.throws(() => receiver.decode(Buffer.from("\n")), protocolError);
    const valid = decoder();
    assert.equal(valid.decode(exact).event, null);
    const events = feed(valid, fixture.notification + fixture.closed);
    assert.deepEqual(events.map((event) => event.kind), ["ready", "notification", "closed"]);
    assert.equal(valid.finish(), null);
    const unfinished = decoder();
    assert.throws(() => feed(unfinished, "x".repeat(MAX_WIRE_BYTES + 1)), protocolError);
  });

  test("notification end-of-stream distinguishes corrupt UTF-8, partial input and terminal frames", () => {
    const invalid = decoder();
    feed(invalid, Buffer.from([0xff]));
    assert.throws(() => invalid.finish(), protocolError);
    const partial = decoder();
    feed(partial, Buffer.from([0xf0, 0x9f]));
    assert.throws(() => partial.finish(), { code: "NOTIFICATION_TRANSPORT" });
    const receiver = decoder();
    feed(receiver, fixture.ready + fixture.closed);
    assert.throws(() => feed(receiver, ": after terminal\n\n"), protocolError);
    const truncated = decoder();
    feed(truncated, fixture.ready + fixture.closed.slice(0, -1));
    assert.throws(() => truncated.finish(), { code: "NOTIFICATION_TRANSPORT" });
  });

  test("notification payloads and cumulative streams retain per-frame bounds and private diagnostics", () => {
    const receiver = decoder();
    feed(receiver, fixture.ready);
    for (let sequence = 1; sequence <= 400; sequence += 1) {
      const event = feed(receiver, wire("notification", { ...data(fixture.notification), sequence: String(sequence) }))[0];
      assert.equal(event.sequence, BigInt(sequence));
      assert.ok(!inspect(event).includes(fixture.expected_payload));
      assert.equal(JSON.stringify(event), "{}");
    }
    feed(receiver, fixture.closed);
    assert.equal(receiver.finish(), null);
    const maximum = decoder();
    feed(maximum, fixture.ready);
    assert.equal(feed(maximum, wire("notification", { ...data(fixture.notification), payload: "x".repeat(7999) }))[0].payload.length, 7999);
    notificationFailure({ payload: "x".repeat(8000) });
    const error = new NotificationError("NOTIFICATION_TRANSPORT", { diagnostic: "PRIVATE_TOKEN_payload" });
    assert.ok(!inspect(error).includes("PRIVATE"));
    assert.ok(!JSON.stringify(error).includes("PRIVATE"));
    assert.equal(error.diagnostic, "PRIVATE_TOKEN_payload");
    assert.equal(new NotificationEvent({ kind: "notification", sequence: (1n << 64n) - 1n }).sequence, 18446744073709551615n);
  });

  test("notification remote retry flags cannot authorize retry of revoked or unknown authority", () => {
    for (const [code, expected, retryable] of [
      ["NOTIFICATION_SOURCE_UNAVAILABLE", "NOTIFICATION_SOURCE_UNAVAILABLE", true],
      ["NOTIFICATION_AUTHORITY_REVOKED", "NOTIFICATION_AUTHORITY_REVOKED", false],
      ["PRIVATE_UNKNOWN_FAILURE", "NOTIFICATION_PROTOCOL", false],
    ]) {
      const receiver = decoder();
      feed(receiver, fixture.ready);
      const error = feed(receiver, wire("error", { ...data(fixture.error), code, retryable: true }))[0].error;
      assert.equal(error.code, expected);
      assert.equal(error.retryable, retryable);
      assert.ok(!inspect(error).includes("PRIVATE"));
      assert.equal(receiver.finish(), null);
    }
  });
}
