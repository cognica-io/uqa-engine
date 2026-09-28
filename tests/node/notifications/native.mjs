//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Worker } from "node:worker_threads";
import { once } from "node:events";
import { execFile } from "node:child_process";
import { promisify, inspect } from "node:util";

const execute = promisify(execFile);
const options = { maxActiveSubscriptions: 8, maxChannels: 2, maxQueuedNotifications: 16,
  maxQueuedBytes: 65536, maxRegistryEntriesPerPoll: 8 };
const kind = (code) => (error) => error.code === "NOTIFICATION_" + code;

export function registerNativeNotificationTests(uqa, packagePath) {
  test("native notification values preserve Unicode, committed order and transaction-local deduplication", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    const sub = await engine.subscribeNotifications(["jobs", "작업"], options);
    try {
      assert.ok(sub instanceof uqa.NotificationSubscription);
      assert.equal(sub.requestId, null);
      assert.match(sub.epoch, /^[0-9a-f-]{36}$/);
      await engine.sql("BEGIN");
      await engine.sql("SELECT pg_notify($1, $2)", ["작업", "{\"😀\":\"값\"}\n"]);
      await engine.sql("SELECT pg_notify($1, $2)", ["작업", "{\"😀\":\"값\"}\n"]);
      await engine.sql("SAVEPOINT discarded");
      await engine.sql("NOTIFY jobs, 'rolled-back'");
      await engine.sql("ROLLBACK TO discarded");
      await engine.sql("COMMIT");
      await engine.sql("BEGIN"); await engine.sql("NOTIFY jobs, 'whole-rollback'"); await engine.sql("ROLLBACK");
      await engine.sql("NOTIFY jobs, 'after'");
      const first = await sub.nextEvent(); const second = await sub.nextEvent();
      assert.equal(first.sequence, 1n); assert.equal(first.channel, "작업"); assert.equal(first.payload, "{\"😀\":\"값\"}\n");
      assert.equal(first.epoch, sub.epoch); assert.equal(first.requestId, null);
      assert.ok(Number.isInteger(first.processId)); assert.equal(first.cause, null);
      assert.equal(second.sequence, 2n); assert.equal(second.payload, "after");
      assert.ok(Object.isFrozen(first));
      assert.equal(inspect(first).includes("값"), false);
      assert.equal(JSON.stringify(first), "{}");
    } finally { await sub.close(); engine.close(); }
  });

  test("native notification overlapping registrations retain independent boundaries and close", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    const first = await engine.subscribeNotifications(["jobs"], options);
    const second = await engine.subscribeNotifications(["jobs", "other"], options);
    let late;
    try {
      await engine.sql("NOTIFY jobs, 'before-late'");
      late = await engine.subscribeNotifications(["jobs"], options);
      await engine.sql("NOTIFY jobs, 'after-late'");
      assert.equal((await first.nextEvent()).payload, "before-late");
      assert.equal((await second.nextEvent()).payload, "before-late");
      assert.equal((await late.nextEvent()).payload, "after-late");
      assert.equal((await first.nextEvent()).sequence, 2n);
      assert.equal((await second.nextEvent()).sequence, 2n);
      await first.close();
      await engine.sql("NOTIFY other, 'independent'");
      assert.equal((await second.nextEvent()).payload, "independent");
      assert.notEqual(first.epoch, second.epoch);
    } finally { await Promise.all([first.close(), second.close(), late?.close()]); engine.close(); }
  });

  test("native notification readiness neither commits nor consumes the caller transaction", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    engine.sqlSync("BEGIN"); engine.sqlSync("NOTIFY jobs, 'uncommitted'");
    const sub = await engine.subscribeNotifications(["jobs"], options);
    try {
      engine.sqlSync("ROLLBACK"); engine.sqlSync("NOTIFY jobs, 'visible'");
      assert.equal((await sub.nextEvent()).payload, "visible");
      await engine.sql("NOTIFY jobs, 'retained'");
      engine.close();
      assert.equal((await sub.nextEvent()).payload, "retained");
    } finally { await sub.close(); engine.close(); }
  });

  test("native notification options and channels reject coercion, replacement Unicode and duplicate inputs", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    try {
      for (const channels of [[], "jobs", [""], ["a", "a"], ["a\0b"], ["문".repeat(22)], ["\ud800"], ["\udfff"], [42], ["a".repeat(64)]]) {
        await assert.rejects(engine.subscribeNotifications(channels, options), kind("INVALID_REQUEST"));
      }
      for (const value of [0, -1, 0.5, NaN, Infinity, "2", 2n, Number.MAX_SAFE_INTEGER + 1]) {
        await assert.rejects(engine.subscribeNotifications(["jobs"], { ...options, maxQueuedNotifications: value }), kind("INVALID_REQUEST"));
      }
      const oversized = Array(3);
      Object.defineProperty(oversized, "0", { get() { assert.fail("count must be rejected before copying"); } });
      await assert.rejects(engine.subscribeNotifications(oversized, options), kind("INVALID_REQUEST"));
      const channels = ["jobs"]; const pending = engine.subscribeNotifications(channels, options); channels[0] = "changed";
      const sub = await pending;
      try { await engine.sql("NOTIFY jobs, 'original'"); assert.equal((await sub.nextEvent()).payload, "original"); }
      finally { await sub.close(); }
    } finally { engine.close(); }
  });

  test("native notification capacity, overflow and repeated close release actual admission", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    const limits = { ...options, maxActiveSubscriptions: 1, maxQueuedNotifications: 1 };
    const sub = await engine.subscribeNotifications(["jobs"], limits);
    try {
      await assert.rejects(engine.subscribeNotifications(["other"], limits), (error) => error instanceof uqa.NotificationError && kind("CAPACITY")(error));
      await engine.sql("NOTIFY jobs, 'accepted'");
      assert.equal((await sub.nextEvent()).payload, "accepted");
      await engine.sql("NOTIFY jobs, 'unread'"); await engine.sql("NOTIFY jobs, 'overflow'");
      await assert.rejects(sub.nextEvent(), kind("BACKPRESSURE"));
      await Promise.all([sub.close(), sub.close()]);
      const fresh = await engine.subscribeNotifications(["jobs"], limits);
      await fresh.close();
      await assert.rejects(sub.nextEvent(), kind("BACKPRESSURE"));
    } finally { await sub.close(); engine.close(); }
  });

  test("native notification AbortSignal joins registration and active receive cleanup", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine(); const limits = { ...options, maxActiveSubscriptions: 1 };
    try {
      const already = new AbortController(); already.abort();
      await assert.rejects(engine.subscribeNotifications(["jobs"], { ...limits, signal: already.signal }), kind("CANCELLED"));
      const registering = new AbortController();
      const pending = engine.subscribeNotifications(["jobs"], { ...limits, signal: registering.signal });
      registering.abort(); await assert.rejects(pending, kind("CANCELLED"));
      const signal = new AbortController();
      const sub = await engine.subscribeNotifications(["jobs"], { ...limits, signal: signal.signal });
      const receiving = sub.nextEvent();
      await assert.rejects(sub.nextEvent(), kind("INVALID_REQUEST"));
      signal.abort(); await assert.rejects(receiving, kind("CANCELLED"));
      const fresh = await engine.subscribeNotifications(["jobs"], limits);
      const waiting = fresh.next(); await fresh.return();
      assert.deepEqual(await waiting, { done: true, value: undefined });
      await sub.close();
    } finally { engine.close(); }
  });

  test("native notification iterator break and throw join cleanup before reuse", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    const limits = { ...options, maxActiveSubscriptions: 1 };
    let sub;
    try {
      sub = await engine.subscribeNotifications(["jobs"], limits);
      await engine.sql("NOTIFY jobs, 'iterator'");
      for await (const event of sub) { assert.equal(event.payload, "iterator"); break; }
      assert.equal(sub.isClosed, true);
      sub = await engine.subscribeNotifications(["jobs"], limits);
      const marker = new Error("consumer failure");
      await assert.rejects(sub.throw(marker), (error) => error === marker);
      sub = await engine.subscribeNotifications(["jobs"], limits);
      await sub.close();
    } finally { await sub?.close(); engine.close(); }
  });

  test("native notification failure stays visible while explicit cleanup is in flight", { timeout: 30000 }, async () => {
    const engine = new uqa.Engine();
    const sub = await engine.subscribeNotifications(["jobs"], { ...options, maxQueuedNotifications: 1 });
    try {
      await engine.sql("NOTIFY jobs, 'first'"); await engine.sql("NOTIFY jobs, 'overflow'");
      const closing = sub.close();
      await assert.rejects(sub.nextEvent(), kind("BACKPRESSURE"));
      await closing;
      await assert.rejects(sub.nextEvent(), kind("BACKPRESSURE"));
    } finally { await sub.close(); engine.close(); }
  });

  for (const [mode, open] of [
    ["sqlite", (path) => uqa.open(path)],
    ["encrypted", (path) => uqa.openEncrypted(path, "notification-binding-fixture")],
    ["compressed", (path) => uqa.openCompressed(path)],
    ["compressed-encrypted", (path) => uqa.openCompressedEncrypted(path, "notification-binding-fixture")],
  ]) {
    test(`native notification retains the original ${mode} provider after query Engine close`, { timeout: 60000 }, async () => {
      const directory = mkdtempSync(join(tmpdir(), "uqa-node-notification-"));
      let engine; let publisher; let sub;
      try {
        engine = open(join(directory, "database.db")); publisher = engine.newSession();
        sub = await engine.subscribeNotifications(["jobs"], options);
        engine.close();
        await publisher.sql("NOTIFY jobs, 'after-close'");
        assert.equal((await sub.nextEvent()).payload, "after-close");
        await sub.close(); publisher.close();
        rmSync(directory, { recursive: true });
      } finally { await sub?.close(); publisher?.close(); engine?.close(); rmSync(directory, { recursive: true, force: true }); }
    });
  }

  test("native notification idle receivers do not occupy the single libuv worker", { timeout: 30000 }, async () => {
    const script = `
      const assert = require('node:assert/strict');
      const uqa = require(${JSON.stringify(packagePath)});
      (async () => {
        const engine = new uqa.Engine();
        const options = ${JSON.stringify(options)};
        const first = await engine.subscribeNotifications(['jobs'], options);
        const pending = first.nextEvent();
        const second = await engine.subscribeNotifications(['jobs'], options);
        await engine.sql("NOTIFY jobs, 'awake'");
        assert.equal((await pending).payload, 'awake');
        assert.equal((await second.nextEvent()).payload, 'awake');
        await Promise.all([first.close(), second.close()]); engine.close();
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `;
    await execute(process.execPath, ["-e", script], { env: { ...process.env, UV_THREADPOOL_SIZE: "1" }, timeout: 20000 });
  });

  test("native notification pending submissions reserve the original shared capacity", { timeout: 30000 }, async () => {
    const script = `
      const assert = require('node:assert/strict');
      const uqa = require(${JSON.stringify(packagePath)});
      (async () => {
        const engine = new uqa.Engine();
        const options = ${JSON.stringify({ ...options, maxActiveSubscriptions: 1 })};
        const order = [];
        const pending = engine.subscribeNotifications(['jobs'], options);
        const rejected = engine.subscribeNotifications(['other'], { ...options, maxActiveSubscriptions: 64 })
          .then(() => assert.fail('pending admission must reject excess work'), error => {
            assert.equal(error.code, 'NOTIFICATION_CAPACITY'); order.push('capacity');
          });
        const subscription = await pending; order.push('ready');
        await rejected;
        try { assert.deepEqual(order, ['capacity', 'ready']); }
        finally { await subscription.close(); }
        const replacement = await engine.subscribeNotifications(['jobs'], options);
        await replacement.close(); engine.close();
      })().catch(error => { console.error(error); process.exitCode = 1; });
    `;
    await execute(process.execPath, ["-e", script], { env: { ...process.env, UV_THREADPOOL_SIZE: "1" }, timeout: 20000 });
  });

  for (const terminating of [false, true]) {
    test(`native notification Worker ${terminating ? "termination" : "natural exit"} joins native listener cleanup`, { timeout: 30000 }, async () => {
    const directory = mkdtempSync(join(tmpdir(), "uqa-node-notification-worker-"));
    const path = join(directory, "database.db");
    const script = `
      const { parentPort } = require('node:worker_threads');
      const uqa = require(${JSON.stringify(packagePath)});
      (async () => {
        const engine = uqa.open(${JSON.stringify(path)});
        const sub = await engine.subscribeNotifications(['jobs'], ${JSON.stringify({ ...options, maxActiveSubscriptions: 1 })});
        engine.close();
        const waiting = ${terminating ? "sub.nextEvent()" : "Promise.resolve()"};
        parentPort.postMessage('ready');
        await waiting;
      })().catch(error => { throw error; });
    `;
    const worker = new Worker(script, { eval: true });
    const exited = once(worker, "exit");
    try {
      assert.deepEqual(await once(worker, "message"), ["ready"]);
      if (terminating) await worker.terminate();
      const [code] = await exited;
      assert.equal(code, terminating ? 1 : 0);
      const engine = uqa.open(path);
      try {
        const sub = await engine.subscribeNotifications(["jobs"], { ...options, maxActiveSubscriptions: 1 });
        await sub.close();
      } finally { engine.close(); }
      rmSync(directory, { recursive: true });
    } finally { await worker.terminate(); rmSync(directory, { recursive: true, force: true }); }
    });
  }
}
