//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import * as defaultRuntime from "../../crates/uqa-wasm/js/index.mjs";
import { runUnifiedSearch } from "../javascript/unified-search.mjs";

if (typeof document === "undefined") {
  console.log(JSON.stringify(await run()));
}

export async function run(runtime = defaultRuntime) {
  const { Engine, vector } = runtime;
  const engine = await Engine.inMemory();
  try {
    return await runUnifiedSearch(engine, vector);
  } finally {
    await engine.close();
  }
}
