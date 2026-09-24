//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "../../examples/javascript/common.mjs";
import { concurrentIsolationLevels, runConcurrentWriterCase } from "../parity/concurrent_transactions.mjs";

const parameters = new URLSearchParams(location.search);
const key = `uqa-concurrent-transactions/${parameters.get("run") ?? "default"}`;
const saved = sessionStorage.getItem(key);
const report = saved ? JSON.parse(saved) : {
  schema_version: 1,
  run_id: crypto.randomUUID(),
  pages: [],
  completed_cases: [],
  restored_cases: [],
  checkpoint_databases: [],
};
const status = document.querySelector("#status");
const output = document.querySelector("#report");
const reload = document.querySelector("#reload");
reload.addEventListener("click", () => location.reload());

function publish(message) {
  report.status = message;
  status.textContent = message;
  output.textContent = JSON.stringify(report, null, 2);
  sessionStorage.setItem(key, JSON.stringify(report));
}

async function run() {
  assertEqual(crossOriginIsolated, true, "cross-origin isolation");
  assertEqual(typeof indexedDB, "object", "real IndexedDB");
  assertEqual(report.pages.length < 2, true, "exactly one page reload");
  report.pages.push(crypto.randomUUID());
  const { Engine, UQA } = await import(parameters.get("bundle") ?? "../../crates/uqa-wasm/js/index.mjs");
  const response = await fetch("../parity/pg18/concurrent_writes.expected.json", { cache: "no-store" });
  if (!response.ok) throw new Error(`concurrent writer oracle: HTTP ${response.status}`);
  const oracle = await response.json();
  const cases = [];
  for (const [mode, open] of [
    ["sqlite", (path) => Engine.open(path)],
    ["compressed", (path) => Engine.openCompressed(path)],
  ]) {
    for (const [index, isolation] of concurrentIsolationLevels.entries()) {
      for (const schedule of oracle.cases) {
        const name = `${mode}/${isolation}/${schedule.name}`;
        const path = `${UQA.persistDir}/concurrent-${report.run_id}-${mode}-${index}-${schedule.name}.db`;
        cases.push({ name, open, path, schedule, isolation });
      }
    }
  }

  if (report.pages.length === 1) {
    for (const { name, open, path, schedule, isolation } of cases) {
      status.textContent = name;
      await runConcurrentWriterCase(open, path, oracle, schedule, isolation, assertEqual);
      report.completed_cases.push(name);
    }
    // Every Engine and session is closed before the IDBFS checkpoint.
    await UQA.persist();
    report.checkpoint_databases = (await indexedDB.databases()).map(({ name }) => name);
    assertEqual(report.checkpoint_databases.includes(UQA.persistDir), true, "IDBFS checkpoint exists");
    publish("Awaiting page reload");
    reload.disabled = false;
    return;
  }

  assertEqual(report.status, "Awaiting page reload", "completed first-page checkpoint");
  assertEqual(report.completed_cases, cases.map(({ name }) => name), "all overlapping session schedules ran");
  for (const { name, open, path, schedule } of cases) {
    const engine = await open(path);
    try {
      const rows = (await engine.sql(oracle.observe)).rows.map(({ id, value }) => `${id}|${value}`);
      assertEqual(rows, schedule.after_a_end, `${name}: restored committed rows`);
      report.restored_cases.push(name);
    } finally {
      await engine.close();
    }
  }
  assertEqual(report.restored_cases, report.completed_cases, "all closed files restored after module reload");
  publish(`Passed: ${cases.length} schedules and fresh-page restores`);
}

run().catch((error) => {
  report.error = error.stack ?? String(error);
  publish("Failed");
});
