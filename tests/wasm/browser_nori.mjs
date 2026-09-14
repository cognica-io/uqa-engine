//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "../../examples/javascript/common.mjs";
import { checkBindingStep } from "../parity/nori/bindings.core.mjs";

const parameters = new URLSearchParams(location.search);
const feature = parameters.get("nori") ?? "enabled";
assertEqual(["enabled", "disabled"].includes(feature), true, "explicit Nori feature configuration");
const bundle = parameters.get("bundle") ?? "../../crates/uqa-wasm/js/index.mjs";
const { Engine, UQA } = await import(bundle);
const status = document.querySelector("#status");
const output = document.querySelector("#report");
const reload = document.querySelector("#reload");
reload.addEventListener("click", () => location.reload());
document.querySelector("#restart").addEventListener("click", () => {
  parameters.set("run", crypto.randomUUID());
  location.search = parameters;
});
const key = `uqa-nori-browser-verification/${parameters.get("run") ?? "default"}/${feature}`;
const saved = sessionStorage.getItem(key);
const report = saved ? JSON.parse(saved) : {
  schema_version: 1,
  feature,
  run_id: crypto.randomUUID(),
  next_step: 0,
  pages: [],
  completed_steps: [],
  checkpoints: [],
  memory: [],
  analyses: [],
};

function publish(message) {
  report.status = message;
  status.textContent = message;
  output.textContent = JSON.stringify(report, null, 2);
  sessionStorage.setItem(key, JSON.stringify(report));
}

async function measureMemory(label) {
  status.textContent = `Measuring browser memory: ${label}`;
  if (typeof performance.measureUserAgentSpecificMemory !== "function") {
    throw new Error("This browser does not expose measureUserAgentSpecificMemory");
  }
  const memory = await performance.measureUserAgentSpecificMemory();
  report.memory.push({ page: report.pages.length, label, ...memory });
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]));
  }
  return value;
}

async function sha256(text) {
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function fetchText(path) {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
  return response.text();
}

async function measureCorpora(engine) {
  const corpusText = await fetchText("../../crates/uqa-analysis/benches/nori/corpus.json");
  const contract = JSON.parse(await fetchText("../../benchmarks/nori/browser-contract.json"));
  assertEqual(await sha256(corpusText), contract.corpus_sha256, "fixed memory corpus");
  const corpus = JSON.parse(corpusText);
  for (const mode of ["none", "discard", "mixed"]) {
    const analyzer = `browser_nori_${mode}`;
    const config = {
      tokenizer: { type: "nori_tokenizer", decompound_mode: mode },
      token_filters: [{ type: "nori_part_of_speech" }, { type: "nori_readingform" }, { type: "unicode_simple_lowercase" }],
    };
    await engine.sql("SELECT * FROM create_analyzer($1, $2)", [analyzer, JSON.stringify(config)]);
    for (const item of corpus.cases) {
      const input = item.text.repeat(item.repeat);
      let analysis = (await engine.sql("SELECT analysis FROM analyze_text($1, $2)", [analyzer, input])).rows[0].analysis;
      await measureMemory(`retained/${mode}/${item.name}`);
      // Hash after sampling so the complete returned diagnostic stays live during measurement.
      report.analyses.push({
        mode, name: item.name, input_utf8_bytes: new TextEncoder().encode(input).byteLength,
        token_count: analysis.tokens.length, analyzer_fingerprint: analysis.analyzer_fingerprint,
        diagnostic_sha256: await sha256(JSON.stringify(canonical(analysis))),
      });
      analysis = null;
      await measureMemory(`released/${mode}/${item.name}`);
    }
    await engine.sql("SELECT * FROM drop_analyzer($1)", [analyzer]);
  }
  assertEqual(report.analyses, contract.checks, "complete native SQL diagnostic identities");
}

async function run() {
  if (report.status?.startsWith("Passed:")) {
    publish(report.status);
    return;
  }
  assertEqual(crossOriginIsolated, true, "cross-origin isolation");
  assertEqual(typeof indexedDB, "object", "real IndexedDB");
  const pageId = crypto.randomUUID();
  assertEqual(report.pages.includes(pageId), false, "fresh page identity");
  report.pages.push(pageId);
  report.user_agent = navigator.userAgent;
  const fixture = JSON.parse(await fetchText("../parity/nori/bindings.json"));
  const steps = fixture[feature];
  assertEqual(fixture.schema_version, 1, "fixture schema");
  await measureMemory("before_load");
  const path = `${UQA.persistDir}/nori-${report.run_id}.db`;
  let engine = await Engine.open(path);
  try {
    await measureMemory("opened");
    for (let index = report.next_step; index < steps.length; index += 1) {
      const step = steps[index];
      status.textContent = `${index + 1}/${steps.length}: ${step.name}`;
      if (step.reopen) {
        await engine.close();
        engine = null;
        await UQA.persist();
        report.completed_steps.push(step.name);
        report.next_step = index + 1;
        const databases = await indexedDB.databases();
        assertEqual(databases.some((database) => database.name === UQA.persistDir), true, "IDBFS database exists");
        report.checkpoints.push({ next_step: report.next_step, databases, storage: await navigator.storage.estimate() });
        await measureMemory("closed_and_persisted");
        publish("Awaiting page reload");
        reload.disabled = false;
        return;
      }
      await checkBindingStep(engine, fixture, step, assertEqual);
      report.completed_steps.push(step.name);
      if (index === 1 || index === 35 || index === 43) await measureMemory(`after_${step.name}`);
    }
    if (feature === "enabled") await measureCorpora(engine);
  } finally {
    await engine?.close();
  }
  await UQA.persist();
  await measureMemory("finished_and_persisted");
  assertEqual(report.completed_steps, steps.map((step) => step.name), "every fixture step ran once");
  assertEqual(report.pages.length, 1 + steps.filter((step) => step.reopen).length, "real page reload count");
  report.next_step = steps.length;
  report.completed_at_utc = new Date().toISOString();
  publish(`Passed: ${steps.length} steps across ${report.pages.length} page loads`);
}

run().catch((error) => {
  report.error = error.stack ?? String(error);
  publish("Failed");
  console.error(error);
});
