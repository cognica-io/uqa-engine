//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { Engine } from "../../../crates/uqa-wasm/js/index.mjs";

let engine, sub, pending, marker;
async function receive(data) {
  if (data.notify) {
    try {
      await engine.sql("NOTIFY jobs, 'worker-only'");
      const event = await pending;
      await sub.close(); await engine.close();
      postMessage({ marker, epoch: event.epoch, sequence: event.sequence.toString(), payload: event.payload });
    } finally { await sub.close(); await engine.close(); }
  } else {
    engine = await Engine.open(data.path);
    marker = (await engine.sql("SELECT id FROM marker")).rows[0].id;
    sub = await engine.subscribeNotifications(["jobs"], data.options);
    pending = sub.nextEvent();
    postMessage({ ready: true });
  }
}
self.onmessage = ({ data }) => { receive(data).catch((error) => postMessage({ error: error.stack ?? String(error) })); };
