//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

const options = { maxActiveSubscriptions: 1, maxChannels: 1, maxQueuedNotifications: 4,
  maxQueuedBytes: 65536, maxRegistryEntriesPerPoll: 4 };
const equal = (actual, expected) => { if (actual !== expected) throw new Error(`Expected ${expected}, got ${actual}`); };

export async function verifyRuntimeIsolation({ Engine, UQA }) {
  const path = UQA.persistDir + "/notification-worker.db";
  const engine = await Engine.open(path);
  let sub; let worker;
  try {
    await engine.sql("CREATE TABLE IF NOT EXISTS marker (id INTEGER PRIMARY KEY)");
    await engine.sql("DELETE FROM marker"); await engine.sql("INSERT INTO marker VALUES (42)");
    await engine.sql("NOTIFY jobs, 'before-worker'"); await UQA.persist();
    sub = await engine.subscribeNotifications(["jobs"], options);
    let received = false;
    const pending = sub.nextEvent().then((event) => { received = true; return event; });
    worker = new Worker(new URL("./worker.mjs", import.meta.url), { type: "module" });
    const result = new Promise((resolve, reject) => {
      worker.onerror = (event) => reject(new Error(event.message));
      worker.onmessage = ({ data }) => {
        if (data.error) reject(new Error(data.error));
        else if (data.ready) worker.postMessage({ notify: true });
        else resolve(data);
      };
    });
    worker.postMessage({ path, options });
    const event = await result;
    equal(event.marker, 42); equal(event.payload, "worker-only"); equal(event.sequence, "1");
    equal(event.epoch === sub.epoch, false); equal(received, false);
    await engine.sql("NOTIFY jobs, 'original-only'"); equal((await pending).payload, "original-only");
  } finally { worker?.terminate(); await sub?.close(); await engine.close(); }
}
