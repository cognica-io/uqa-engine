//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { Engine } from "../../crates/uqa-wasm/js/index.mjs";
import { runGraphCypher } from "../javascript/graph-cypher.mjs";

if (typeof document === "undefined") {
  console.log(JSON.stringify(await run()));
}

export async function run() {
  const engine = await Engine.inMemory();
  try {
    return await runGraphCypher(engine);
  } finally {
    await engine.close();
  }
}
