//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

const EXAMPLES = new Set([
  "unified-search",
  "vector-knn",
  "graph-cypher",
  "storage-transactions",
  "extensibility",
]);

const output = document.querySelector("#output");
const selected = new URLSearchParams(window.location.search).get("example") ?? "unified-search";

if (!EXAMPLES.has(selected)) {
  output.textContent = `Unknown example: ${selected}`;
} else {
  document.title = `UQA Engine ${selected}`;
  output.textContent = `Running ${selected}...`;
  try {
    const module = await import(`./${selected}.mjs`);
    const result = await module.run();
    output.textContent = JSON.stringify(result, null, 2);
  } catch (error) {
    output.textContent = error instanceof Error ? `${error.stack ?? error.message}` : String(error);
    throw error;
  }
}
