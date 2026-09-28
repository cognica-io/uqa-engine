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

}
