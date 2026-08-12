//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { Engine, vector } from "../../crates/uqa-wasm/js/index.mjs";
import { runUnifiedSearch } from "../javascript/unified-search.mjs";

if (typeof document === "undefined") {
  console.log(JSON.stringify(await run()));
}

export async function run() {
  const engine = await Engine.inMemory();
  try {
    return await runUnifiedSearch(engine, vector);
  } finally {
    await engine.close();
  }
}
