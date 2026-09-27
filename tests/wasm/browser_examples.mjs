//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "../../examples/javascript/common.mjs";
import { verifyVectorKNNReopen } from "../../examples/javascript/vector-knn.mjs";

const examples = ["unified-search", "vector-knn", "graph-cypher", "storage-transactions", "extensibility"];
const parameters = new URLSearchParams(location.search);
const key = `uqa-browser-examples/${parameters.get("run") ?? "default"}`;
const saved = sessionStorage.getItem(key);
const report = saved ? JSON.parse(saved) : {
  schema_version: 1, pages: [], completed_examples: [], status: "Starting",
};

function publish(status) {
  report.status = status;
  sessionStorage.setItem(key, JSON.stringify(report));
  document.querySelector("#status").textContent = status;
  document.querySelector("#report").textContent = JSON.stringify(report, null, 2);
}

async function run() {
  assertEqual(report.pages.length < 2, true, "exactly one fresh page reload");
  report.pages.push(crypto.randomUUID());
  const runtime = await import(parameters.get("bundle") ?? "../../crates/uqa-wasm/js/index.mjs");
  const { Engine, UQA } = runtime;
  if (report.pages.length === 1) {
    for (const name of examples) {
      document.querySelector("#status").textContent = name;
      const scenario = await import(`../../examples/browser/${name}.mjs`);
      const result = await scenario.run(runtime);
      if (name === "vector-knn") {
        report.diskann = { path: result.databasePath, expected: result.afterCommit };
      }
      report.completed_examples.push(name);
    }
    await UQA.persist();
    report.checkpoint_databases = (await indexedDB.databases()).map(({ name }) => name);
    assertEqual(report.checkpoint_databases.includes(UQA.persistDir), true, "closed files were checkpointed");
    publish("Awaiting page reload");
    document.querySelector("#reload").disabled = false;
    return;
  }
  assertEqual(report.status, "Awaiting page reload", "completed first-page checkpoint");
  assertEqual(report.completed_examples, examples, "all five examples ran through this artifact");
  const engine = await Engine.open(report.diskann.path);
  try {
    const rows = await verifyVectorKNNReopen(engine, runtime.vector, report.diskann.expected);
    assertEqual(rows.map(({ id }) => id), [6, 1, 3, 2, 4, 5], "all committed DiskANN rows survived reload");
    assertEqual(rows[0]._score, 1, "literal committed score survived reload");
    report.restored_diskann = true;
  } finally {
    await engine.close();
  }
  publish("Passed: five examples and fresh-page DiskANN restore");
}

run().catch((error) => {
  report.error = error.stack ?? String(error);
  publish("Failed");
});
