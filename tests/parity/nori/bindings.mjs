//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { checkBindingStep } from "./bindings.core.mjs";

const fixture = JSON.parse(readFileSync(new URL("./bindings.json", import.meta.url), "utf8"));
const feature = process.env.UQA_TEST_NORI ?? "enabled";
assert.ok(["enabled", "disabled"].includes(feature), "UQA_TEST_NORI must be enabled or disabled");
export const noriEnabled = feature === "enabled";

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
      await checkBindingStep(engine, fixture, step, assert.deepEqual);
    }
  } finally {
    await engine?.close();
  }
}
