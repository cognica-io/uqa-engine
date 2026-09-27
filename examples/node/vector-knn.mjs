//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createRequire } from "node:module";

import { runVectorKNN, verifyVectorKNNReopen } from "../javascript/vector-knn.mjs";

const require = createRequire(import.meta.url);
const uqa = require("../../crates/uqa-node");
const directory = mkdtempSync(join(tmpdir(), "uqa-node-vector-"));
const path = join(directory, "vectors.db");
try {
  const engine = uqa.open(path);
  let results;
  try {
    results = await runVectorKNN(engine, uqa.vector);
  } finally {
    engine.close();
  }
  const reopened = uqa.open(path);
  try {
    results.reopened = await verifyVectorKNNReopen(reopened, uqa.vector, results.afterCommit);
  } finally {
    reopened.close();
  }
  console.log(JSON.stringify(results));
} finally {
  rmSync(directory, { recursive: true, force: true });
}
