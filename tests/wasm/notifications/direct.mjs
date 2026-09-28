//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

const options = { maxActiveSubscriptions: 8, maxChannels: 2, maxQueuedNotifications: 16,
  maxQueuedBytes: 65536, maxRegistryEntriesPerPoll: 8 };
const equal = (actual, expected) => { if (actual !== expected) throw new Error(`Expected ${String(expected)}, got ${String(actual)}`); };

export async function runDirectNotifications({ Engine, NotificationSubscription, NotificationError }, run) {
  async function rejects(promise, code) {
    let error;
    try { await promise; } catch (caught) { error = caught; }
    if (!(error instanceof NotificationError)) throw new Error("Expected a typed notification failure");
    equal(error.code, "NOTIFICATION_" + code);
  }
  await run("direct WASM exact values, commit order, deduplication and rollback", async () => {
    const engine = await Engine.inMemory();
    const sub = await engine.subscribeNotifications(["jobs", "작업"], options);
    try {
      equal(sub instanceof NotificationSubscription, true); equal(sub.requestId, null);
      await engine.sql("BEGIN");
      for (let i = 0; i < 2; i += 1) await engine.sql("SELECT pg_notify($1, $2)", ["작업", "{\"😀\":\"값\"}\n"]);
      await engine.sql("SAVEPOINT discarded"); await engine.sql("NOTIFY jobs, 'savepoint'");
      await engine.sql("ROLLBACK TO discarded"); await engine.sql("COMMIT");
      await engine.sql("BEGIN"); await engine.sql("NOTIFY jobs, 'rollback'"); await engine.sql("ROLLBACK");
      await engine.sql("NOTIFY jobs, 'after'");
      const event = await sub.nextEvent();
      equal(event.sequence, 1n); equal(event.channel, "작업"); equal(event.payload, "{\"😀\":\"값\"}\n");
      equal(event.epoch, sub.epoch); equal(event.requestId, null); equal(event.cause, null);
      equal(Number.isInteger(event.processId), true); equal(Object.isFrozen(event), true); equal(JSON.stringify(event), "{}");
      const second = await sub.nextEvent(); equal(second.sequence, 2n); equal(second.payload, "after");
    } finally { await sub.close(); await engine.close(); }
  });
  await run("direct WASM independent registration boundaries and all-channel readiness", async () => {
    const engine = await Engine.inMemory(); const subscriptions = [];
    try {
      const first = await engine.subscribeNotifications(["jobs"], options); subscriptions.push(first);
      await engine.sql("NOTIFY jobs, 'early'");
      const second = await engine.subscribeNotifications(["jobs", "other"], options); subscriptions.push(second);
      await engine.sql("NOTIFY jobs, 'shared'"); await engine.sql("NOTIFY other, 'second'");
      equal((await first.nextEvent()).payload, "early"); equal((await second.nextEvent()).payload, "shared");
      equal((await first.nextEvent()).sequence, 2n); equal((await second.nextEvent()).payload, "second");
      equal(first.epoch === second.epoch, false); await first.close();
      await engine.sql("NOTIFY jobs, 'independent'"); equal((await second.nextEvent()).payload, "independent");
    } finally { for (const sub of subscriptions) await sub.close(); await engine.close(); }
  });
  await run("direct WASM readiness preserves the caller transaction", async () => {
    const engine = await Engine.inMemory(); let sub;
    try {
      await engine.sql("BEGIN"); await engine.sql("NOTIFY jobs, 'uncommitted'");
      sub = await engine.subscribeNotifications(["jobs"], options);
      await engine.sql("ROLLBACK"); await engine.sql("NOTIFY jobs, 'visible'");
      equal((await sub.nextEvent()).payload, "visible");
    } finally { await sub?.close(); await engine.close(); }
  });
  await run("direct WASM input validation and channel snapshot", async () => {
    const engine = await Engine.inMemory();
    try {
      for (const channels of [[], "jobs", [""], ["a", "a"], ["a\0b"], ["문".repeat(22)], ["\ud800"], ["\udfff"], [42], ["a".repeat(64)]]) {
        await rejects(engine.subscribeNotifications(channels, options), "INVALID_REQUEST");
      }
      for (const value of [0, -1, 0.5, NaN, Infinity, "2", 2n, 4294967296]) {
        await rejects(engine.subscribeNotifications(["jobs"], { ...options, maxQueuedNotifications: value }), "INVALID_REQUEST");
      }
      const oversized = Array(3);
      Object.defineProperty(oversized, "0", { get() { throw new Error("count rejected after copying"); } });
      await rejects(engine.subscribeNotifications(oversized, options), "INVALID_REQUEST");
      const channels = ["jobs"]; const pending = engine.subscribeNotifications(channels, options); channels[0] = "changed";
      const sub = await pending;
      try { await engine.sql("NOTIFY jobs, 'original'"); equal((await sub.nextEvent()).payload, "original"); }
      finally { await sub.close(); }
    } finally { await engine.close(); }
  });
  await run("direct WASM cancellation before readiness releases capacity", async () => {
    const engine = await Engine.inMemory(); const limits = { ...options, maxActiveSubscriptions: 1 };
    try {
      const already = new AbortController(); already.abort();
      await rejects(engine.subscribeNotifications(["jobs"], { ...limits, signal: already.signal }), "CANCELLED");
      const signal = new AbortController();
      const pending = engine.subscribeNotifications(["jobs"], { ...limits, signal: signal.signal });
      signal.abort(); await rejects(pending, "CANCELLED");
      const sub = await engine.subscribeNotifications(["jobs"], limits);
      try { await rejects(engine.subscribeNotifications(["other"], { ...options, maxActiveSubscriptions: 64 }), "CAPACITY"); }
      finally { await sub.close(); }
    } finally { await engine.close(); }
  });
  await run("direct WASM idle receive permits event-loop progress and one consumer", async () => {
    const engine = await Engine.inMemory(); const signal = new AbortController();
    const sub = await engine.subscribeNotifications(["jobs"], { ...options, signal: signal.signal });
    try {
      const pending = sub.nextEvent();
      await rejects(sub.nextEvent(), "INVALID_REQUEST");
      const message = new MessageChannel();
      await new Promise((done) => { message.port1.onmessage = done; message.port2.postMessage("progress"); });
      message.port1.close(); message.port2.close();
      await engine.sql("NOTIFY jobs, 'awake'"); equal((await pending).payload, "awake");
      const cancelled = rejects(sub.nextEvent(), "CANCELLED"); signal.abort(); await cancelled;
      equal(sub.isClosed, true); await rejects(sub.nextEvent(), "CANCELLED");
    } finally { await sub.close(); await engine.close(); }
  });
  await run("direct WASM explicit close joins pending receipt and iterator cleanup", async () => {
    const engine = await Engine.inMemory(); const limits = { ...options, maxActiveSubscriptions: 1 }; let sub;
    try {
      sub = await engine.subscribeNotifications(["jobs"], limits);
      const pending = sub.next(); await Promise.all([sub.close(), sub.close()]); equal((await pending).done, true);
      sub = await engine.subscribeNotifications(["jobs"], limits);
      await engine.sql("NOTIFY jobs, 'break'");
      for await (const event of sub) { equal(event.payload, "break"); break; }
      equal(sub.isClosed, true);
      sub = await engine.subscribeNotifications(["jobs"], limits);
      const marker = new Error("consumer failure"); let caught;
      try { await sub.throw(marker); } catch (error) { caught = error; } equal(caught, marker);
      sub = await engine.subscribeNotifications(["jobs"], limits);
    } finally { await sub?.close(); await engine.close(); }
  });
  await run("direct WASM overflow survives cleanup and isolates a healthy listener", async () => {
    const engine = await Engine.inMemory(); let slow; let healthy;
    try {
      slow = await engine.subscribeNotifications(["jobs"], { ...options, maxQueuedNotifications: 1 });
      healthy = await engine.subscribeNotifications(["jobs"], options);
      await engine.sql("NOTIFY jobs, 'one'"); await engine.sql("NOTIFY jobs, 'two'");
      equal(slow.isClosed, true); const closing = slow.close(); await rejects(slow.nextEvent(), "BACKPRESSURE"); await closing;
      await rejects(slow.nextEvent(), "BACKPRESSURE");
      equal((await healthy.nextEvent()).sequence, 1n); equal((await healthy.nextEvent()).sequence, 2n);
      await healthy.close();
      const replacement = await engine.subscribeNotifications(["jobs"], { ...options, maxActiveSubscriptions: 1 }); await replacement.close();
    } finally { await slow?.close(); await healthy?.close(); await engine.close(); }
  });
  for (const mode of ["memory", "sqlite", "compressed"]) {
    await run("direct WASM retains the original " + mode + " source after query close", async () => {
      const path = "/notification-" + mode + ".db";
      const engine = mode === "memory" ? await Engine.inMemory() : mode === "sqlite" ? await Engine.open(path) : await Engine.openCompressed(path);
      const publisher = mode === "memory" ? engine : await engine.newSession();
      const sub = await engine.subscribeNotifications(["jobs"], options);
      try {
        if (mode === "memory") await engine.sql("NOTIFY jobs, 'retained'");
        await engine.close();
        engine.module = {}; engine.handle = -1;
        const pending = sub.nextEvent();
        if (mode !== "memory") await publisher.sql("NOTIFY jobs, 'retained'");
        equal((await pending).payload, "retained");
      } finally { await sub.close(); await publisher.close(); }
    });
  }
}
