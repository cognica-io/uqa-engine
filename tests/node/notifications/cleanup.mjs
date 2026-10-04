//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-test resource ownership on every supported Node.js runtime, including Node 16.

import { test } from "node:test";

export function testWithCleanup(name, options, run) {
  if (typeof options === "function") { run = options; options = {}; }
  return test(name, options, async (context) => {
    const actions = [];
    let closing;
    const close = () => closing ??= (async () => {
      const errors = [];
      while (actions.length > 0) {
        try { await actions.pop()(); } catch (error) { errors.push(error); }
      }
      if (errors.length === 1) throw errors[0];
      if (errors.length > 1) throw new AggregateError(errors, "notification fixture cleanup failed");
    })();
    const abort = () => { void close().catch(() => {}); };
    context.signal.addEventListener("abort", abort, { once: true });
    const errors = [];
    try {
      await run((action) => {
        if (closing) throw new Error("notification fixture is already closing");
        actions.push(action);
      });
    } catch (error) { errors.push(error); }
    finally {
      context.signal.removeEventListener("abort", abort);
      try { await close(); } catch (error) { errors.push(error); }
    }
    if (errors.length === 1) throw errors[0];
    if (errors.length > 1) throw new AggregateError(errors, "notification test and cleanup failed");
  });
}
