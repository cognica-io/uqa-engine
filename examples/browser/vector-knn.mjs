//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import * as defaultRuntime from "../../crates/uqa-wasm/js/index.mjs";
import { runVectorKNN, verifyVectorKNNReopen } from "../javascript/vector-knn.mjs";

if (typeof document === "undefined") {
  console.log(JSON.stringify(await run()));
}

export async function run(runtime = defaultRuntime) {
  const { Engine, UQA, vector } = runtime;
  const path = `${UQA.persistDir}/vector-knn-${crypto.randomUUID()}.db`;
  const engine = await Engine.open(path);
  let results;
  try {
    results = await runVectorKNN(engine, vector);
  } finally {
    await engine.close();
  }
  if (typeof indexedDB !== "undefined") await UQA.persist();
  const reopened = await Engine.open(path);
  try {
    results.reopened = await verifyVectorKNNReopen(reopened, vector, results.afterCommit);
  } finally {
    await reopened.close();
  }
  return { ...results, databasePath: path };
}
