//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { createRequire } from "node:module";

import { runGraphCypher } from "../javascript/graph-cypher.mjs";

const require = createRequire(import.meta.url);
const { Engine } = require("../../crates/uqa-node");

const engine = new Engine();
try {
  console.log(JSON.stringify(await runGraphCypher(engine)));
} finally {
  engine.close();
}
