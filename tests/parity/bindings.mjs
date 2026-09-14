//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { checkBindingStep } from "./bindings.core.mjs";

export const bindingFeatures = ["nori", "kuromoji"].map((language) => {
  const variable = `UQA_TEST_${language.toUpperCase()}`;
  const feature = process.env[variable] ?? "enabled";
  assert.ok(["enabled", "disabled"].includes(feature), `${variable} must be enabled or disabled`);
  return [language, feature === "enabled"];
});
export const noriEnabled = bindingFeatures.find(([language]) => language === "nori")[1];

export async function runBindings(language, open, path, enabled) {
  assert.ok(bindingFeatures.some(([name]) => name === language), "known fixture language");
  assert.equal(typeof enabled, "boolean", "explicit feature configuration");
  const fixture = JSON.parse(readFileSync(new URL(`./${language}/bindings.json`, import.meta.url), "utf8"));
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
