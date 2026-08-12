//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const { Engine } = require("../../crates/uqa-node");

const engine = new Engine();
try {
  await engine.sql("CREATE TABLE samples (grp TEXT, label TEXT, value INTEGER)");
  await engine.sql(
    "INSERT INTO samples (grp, label, value) VALUES " +
      "('a', ' SQL Manual ', 1), ('a', 'Node JS', 2), ('b', 'Browser WASM', 3)",
  );
  const options = { volatility: "immutable", mayMutateEngine: false };
  engine.registerScalarFunction(
    "normalize_label",
    (value) => value.trim().toLowerCase().replaceAll(" ", "-"),
    options,
  );
  engine.registerTableFunction(
    "repeat_rows",
    (label, times) => ({
      columns: ["label", "idx"],
      rows: Array.from({ length: times }, (_, index) => [label, index]),
    }),
    options,
  );
  engine.registerAggregateFunction(
    "sum_squares",
    () => ({
      total: 0,
      observe(value) {
        if (value !== null) {
          this.total += value * value;
        }
      },
      finish() {
        return this.total;
      },
    }),
    options,
  );

  const results = {
    scalar: (
      await engine.sql("SELECT normalize_label(label) AS label FROM samples ORDER BY value")
    ).rows,
    table: (
      await engine.sql(
        "SELECT label, idx FROM repeat_rows('row', 3) AS r(label, idx) ORDER BY idx",
      )
    ).rows,
    aggregate: (
      await engine.sql(
        "SELECT grp, sum_squares(value) AS total FROM samples GROUP BY grp ORDER BY grp",
      )
    ).rows,
  };
  assert.deepEqual(results, expectedResults());
  console.log(JSON.stringify(results));
} finally {
  engine.close();
}

function expectedResults() {
  return {
    scalar: [{ label: "sql-manual" }, { label: "node-js" }, { label: "browser-wasm" }],
    table: [
      { label: "row", idx: 0 },
      { label: "row", idx: 1 },
      { label: "row", idx: 2 },
    ],
    aggregate: [
      { grp: "a", total: 5 },
      { grp: "b", total: 9 },
    ],
  };
}
