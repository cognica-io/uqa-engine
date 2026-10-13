//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { inspect } from "node:util";

const valid = { kind: "batch_statements", limit: 128, actual: 200, actual_is_lower_bound: false };

async function origin(code, limit, run, requestId = "limit-request") {
  let requests = 0;
  const instance = createServer(async (request, response) => {
    for await (const _chunk of request) { /* Consume the actual posted body. */ }
    requests += 1;
    response.writeHead(413, { "content-type": "application/json", "x-request-id": "limit-request" });
    response.end(JSON.stringify({ error: { code, message: "private server query", limit }, request_id: requestId }));
  });
  await new Promise((resolve) => instance.listen(0, "127.0.0.1", resolve));
  try {
    await run("http://127.0.0.1:" + instance.address().port);
    assert.equal(requests, 1, "a rejection must never cause automatic replay");
  } finally {
    instance.closeAllConnections?.();
    await new Promise((resolve) => instance.close(resolve));
  }
}

export function registerHTTPLimitTests(label, HttpEngine, HttpEngineError) {
  test(label + " safe HTTP limits preserve counts, lower bounds and response identity", async () => {
    const cases = [
      ["TOO_MANY_STATEMENTS", valid], ["TOO_MANY_STATEMENTS", { ...valid, actual: 400 }],
      ["SQL_TOO_LARGE", { ...valid, kind: "sql_bytes" }],
      ["TOO_MANY_PARAMETERS", { ...valid, kind: "parameters", limit: 0, actual: 1 }],
      ["RESULT_TOO_LARGE", { ...valid, kind: "result_bytes" }],
      ["REQUEST_BODY_TOO_LARGE", { ...valid, kind: "request_body_bytes", actual: 129, actual_is_lower_bound: true }],
      ...["query_admission", "authentication_admission"].map((kind) => ["NODE_BUSY",
        { ...valid, kind, limit: 8, actual: 9, actual_is_lower_bound: true, executing_limit: 16 }]),
    ];
    for (const [code, limit] of cases) {
      await origin(code, limit, async (url) => {
        const engine = new HttpEngine(url, "limit-token");
        await assert.rejects(() => engine.sqlBatch(Array.from({ length: limit.actual }, () => ["SELECT 1", []])), (error) => {
          assert.ok(error instanceof HttpEngineError);
          assert.equal(error.status, 413); assert.equal(error.code, code);
          assert.equal(error.requestId, "limit-request");
          assert.deepEqual(error.limit, { kind: limit.kind, limit: limit.limit, actual: limit.actual,
            actualIsLowerBound: limit.actual_is_lower_bound,
            ...(limit.executing_limit === undefined ? {} : { executingLimit: limit.executing_limit }) });
          assert.ok(Object.isFrozen(error.limit));
          assert.ok(error.message.includes(limit.kind + " limit " + limit.limit));
          assert.ok(error.message.includes("observed " + (limit.actual_is_lower_bound ? "at least " : "") + limit.actual));
          assert.ok(!inspect(error).includes("private server query")); return true;
        });
      });
    }
  });

  test(label + " malformed HTTP limits preserve the original private error", async () => {
    const invalid = [undefined, null, [], "private server query",
      ...Object.entries({ kind: "private server query", limit: -1, actual: 128, actual_is_lower_bound: true,
        hint: "private server query", executing_limit: 16 }).map(([key, value]) => ({ ...valid, [key]: value })),
      ...[0, 128.5, "128"].map((limit) => ({ ...valid, limit })),
      { ...valid, kind: "sql_bytes" }, { ...valid, actual: Number.MAX_SAFE_INTEGER + 1 },
      { ...valid, actual_is_lower_bound: "false" },
      ...Object.keys(valid).map((key) => { const value = { ...valid }; delete value[key]; return value; }),
    ];
    for (const limit of invalid) {
      await origin("TOO_MANY_STATEMENTS", limit, async (url) => {
        await assert.rejects(() => new HttpEngine(url, "limit-token").sqlBatch([["SELECT 1", []]]), (error) => {
          assert.equal(error.code, "TOO_MANY_STATEMENTS"); assert.equal(error.status, 413);
          assert.equal(error.limit, undefined); assert.equal(error.requestId, "limit-request");
          assert.equal(error.message, "UQA returned 413 with code TOO_MANY_STATEMENTS");
          assert.ok(!inspect(error).includes("private server query")); return true;
        });
      });
    }
    await origin("TOO_MANY_STATEMENTS", valid, async (url) => {
      await assert.rejects(() => new HttpEngine(url, "limit-token").sqlBatch([["SELECT 1", []]]), /request IDs do not match/);
    }, "different-request");
  });
}
