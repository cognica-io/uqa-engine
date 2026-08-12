//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { createRequire } from "node:module";

import { runVectorKNN } from "../javascript/vector-knn.mjs";

const require = createRequire(import.meta.url);
const uqa = require("../../crates/uqa-node");

const engine = new uqa.Engine();
try {
  console.log(JSON.stringify(await runVectorKNN(engine, uqa.vector)));
} finally {
  engine.close();
}
