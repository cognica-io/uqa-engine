//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const fixture = JSON.parse(readFileSync(new URL("./bindings.json", import.meta.url), "utf8"));

export async function runNoriBindings(open, path, enabled = true) {
  assert.equal(fixture.schema_version, 1);
  let engine = await open(path);
  try {
    for (const step of fixture[enabled ? "enabled" : "disabled"]) {
      if (step.reopen) {
        await engine.close();
        engine = null;
        engine = await open(path);
        continue;
      }
      if (step.error_contains) {
        await assert.rejects(engine.sql(step.sql, step.params ?? []), (error) => {
          assert.ok(error.message.includes(step.error_contains), `${step.name}: ${error.message}`);
          return true;
        }, step.name);
      } else {
        const result = await engine.sql(step.sql, step.params ?? []);
        if (step.rows_ref || step.rows) {
          assert.deepEqual(result.rows, step.rows_ref ? fixture[step.rows_ref] : step.rows, step.name);
        }
      }
    }
  } finally {
    await engine?.close();
  }
}
