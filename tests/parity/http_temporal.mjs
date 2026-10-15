//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFileSync } from "node:fs";

export const temporalOracle = JSON.parse(readFileSync(new URL("./http_temporal.json", import.meta.url), "utf8"));

export function registerHTTPTemporalTests(platform, HttpEngine, { browser = false } = {}) {
  test(`${platform} HTTP temporal carriers retain PostgreSQL text and precision boundaries`, async () => {
    const server = createServer(async (request, response) => {
      let body = "";
      for await (const chunk of request) body += chunk;
      const fixture = temporalOracle.cases[Number(JSON.parse(body).sql)];
      response.writeHead(200, { "content-type": "application/json", "x-request-id": "temporal-oracle" });
      response.end('{"request_id":"temporal-oracle","columns":["v","nested"],"affected_rows":0,"rows":[{"v":'
        + fixture.wire + ',"nested":[' + fixture.wire + ']}]}');
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    try {
      const engine = new HttpEngine(`http://127.0.0.1:${server.address().port}`, "test-token");
      for (const [index, fixture] of temporalOracle.cases.entries()) {
        if (browser && !fixture.browser_safe) {
          await assert.rejects(engine.sql(String(index)), /browser safe range/, fixture.sql);
        } else {
          const result = await engine.sql(String(index));
          assert.deepEqual(result.rows[0], { v: fixture.expected, nested: [fixture.expected] }, fixture.sql);
        }
      }
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  });
}
