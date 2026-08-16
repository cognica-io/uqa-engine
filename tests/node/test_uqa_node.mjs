//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";

const require = createRequire(import.meta.url);
const uqa = require("../../crates/uqa-node");

async function readRequestJSON(request) {
  const chunks = [];
  for await (const chunk of request) {
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function sendJSON(response, requestId, payload) {
  const body = JSON.stringify(payload);
  response.writeHead(200, {
    "content-length": Buffer.byteLength(body),
    "content-type": "application/json",
    "x-request-id": requestId,
  });
  response.end(body);
}

async function withHTTPServer(run) {
  const server = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, "Bearer uqa_db_test");
    const payload = await readRequestJSON(request);
    if (request.url === "/v1/sql") {
      assert.deepEqual(payload.params[0], { type: "int64", value: 7 });
      sendJSON(response, "qry_node", {
        columns: ["answer", "payload"],
        rows: [{ answer: 7, payload: { $uqa_type: "bytes", hex: "00ff" } }],
        affected_rows: 0,
        request_id: "qry_node",
      });
      return;
    }
    if (request.url === "/v1/sql/batch") {
      assert.equal(payload.statements.length, 2);
      sendJSON(response, "qry_node_batch", {
        results: [
          { columns: [], rows: [], affected_rows: 1 },
          { columns: ["n"], rows: [{ n: 1 }], affected_rows: 0 },
        ],
        request_id: "qry_node_batch",
      });
      return;
    }
    if (request.url === "/v1/sql/stream") {
      const body = [
        {
          type: "metadata",
          columns: ["n"],
          row_count: 1,
          spilled_to_disk: false,
          request_id: "qry_node_stream",
        },
        { type: "row", row: { n: 1 } },
        { type: "complete", row_count: 1, request_id: "qry_node_stream" },
      ].map((frame) => `${JSON.stringify(frame)}\n`).join("");
      response.writeHead(200, {
        "content-length": Buffer.byteLength(body),
        "content-type": "application/x-ndjson",
        "x-request-id": "qry_node_stream",
      });
      response.end(body);
      return;
    }
    response.writeHead(404);
    response.end();
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.notEqual(address, null);
  try {
    await run(`http://127.0.0.1:${address.port}/`);
  } finally {
    await new Promise((resolve, reject) => {
      server.close((error) => error === undefined ? resolve() : reject(error));
    });
  }
}

async function withConcurrentHTTPServer(run) {
  let activeRequests = 0;
  let peakRequests = 0;
  const server = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, "Bearer uqa_db_test");
    const payload = await readRequestJSON(request);
    assert.equal(request.url, "/v1/sql");
    assert.equal(payload.sql, "SELECT 1");
    assert.deepEqual(payload.params, []);
    activeRequests += 1;
    peakRequests = Math.max(peakRequests, activeRequests);
    await new Promise((resolve) => setTimeout(resolve, 150));
    activeRequests -= 1;
    sendJSON(response, "qry_node_concurrent", {
      columns: ["n"],
      rows: [{ n: 1 }],
      affected_rows: 0,
      request_id: "qry_node_concurrent",
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  assert.notEqual(address, null);
  try {
    await run(`http://127.0.0.1:${address.port}/`, () => peakRequests);
  } finally {
    await new Promise((resolve, reject) => {
      server.close((error) => error === undefined ? resolve() : reject(error));
    });
  }
}

test("HTTP engine executes SQL, atomic batches, and streams", async () => {
  await withHTTPServer(async (origin) => {
    const engine = new uqa.HttpEngine(origin, "uqa_db_test");
    const execution = await engine.sqlWithMetadata("SELECT $1", [7]);
    assert.equal(execution.requestId, "qry_node");
    assert.equal(execution.result.rows[0].answer, 7);
    assert.deepEqual(execution.result.rows[0].payload, Buffer.from([0, 255]));

    const batch = await engine.sqlBatchWithMetadata([
      ["INSERT INTO t VALUES (1)", []],
      ["SELECT 1 AS n", []],
    ]);
    assert.equal(batch.requestId, "qry_node_batch");
    assert.deepEqual(batch.results.at(-1).rows, [{ n: 1 }]);

    const stream = await engine.sqlStream("SELECT 1 AS n");
    assert.equal(stream.requestId, "qry_node_stream");
    const frames = [];
    for await (const frame of stream) {
      frames.push(frame);
    }
    assert.deepEqual(frames.map((frame) => frame.type), ["metadata", "row", "complete"]);
    assert.deepEqual(frames[1].row, { n: 1 });
  });
});

test("HTTP engine does not serialize concurrent requests onto the libuv worker pool", async () => {
  await withConcurrentHTTPServer(async (origin, peakRequests) => {
    const engine = new uqa.HttpEngine(origin, "uqa_db_test");
    const results = await Promise.all(
      Array.from({ length: 8 }, () => engine.sql("SELECT 1", [])),
    );
    assert.equal(peakRequests(), 8);
    assert.ok(results.every((result) => result.rows[0].n === 1));
  });
});

test("CommonJS and ESM package exports agree", async () => {
  const esm = await import("../../crates/uqa-node/api.js");
  assert.equal(esm.Engine, uqa.Engine);
  assert.equal(esm.HttpEngine, uqa.HttpEngine);
  assert.equal(esm.HttpSQLStream, uqa.HttpSQLStream);
  assert.equal(esm.vector, uqa.vector);
  assert.equal(esm.JSFunctionVolatility, uqa.JSFunctionVolatility);
});

test("sql, params, vector, tensor, and cypher surfaces", async () => {
  const engine = new uqa.Engine();
  const created = await engine.sql(
    "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, body TEXT, embedding VECTOR(3), chunks TENSOR(3))"
  );
  assert.equal(created.affectedRows, 0);

  await engine.sql(
    "INSERT INTO notes (id, title, body, embedding, chunks) VALUES ($1, $2, $3, $4, $5)",
    [
      1,
      "rust database",
      "rust query engine",
      uqa.vector([1.0, 0.0, 0.0]),
      uqa.tensor([
        [1.0, 0.0, 0.0],
        [0.8, 0.1, 0.0],
      ]),
    ]
  );
  await engine.sql(
    "INSERT INTO notes (id, title, body, embedding, chunks) VALUES ($1, $2, $3, $4, $5)",
    [
      2,
      "node client",
      "typescript package binding",
      uqa.vector(new Float32Array([0.0, 1.0, 0.0])),
      uqa.tensor([[0.0, 1.0, 0.0]]),
    ]
  );
  await engine.sql("CREATE INDEX notes_body_idx ON notes USING gin (body)");

  const text = await engine.sql(
    "SELECT id, _score FROM notes WHERE text_match(body, 'rust') ORDER BY _score DESC"
  );
  assert.equal(text.rows[0].id, 1);

  const viaParam = await engine.sql(
    "SELECT id FROM notes WHERE knn_match(embedding, $1, 1)",
    [uqa.SQLParam.vector([1.0, 0.0, 0.0])]
  );
  assert.equal(viaParam.rows[0].id, 1);

  const direct = await engine.knnSearch(
    "notes",
    "embedding",
    new Float32Array([1.0, 0.0, 0.0]),
    1
  );
  assert.equal(direct[0].docId, 1);

  const directSync = engine.knnSearchSync("notes", "embedding", [1.0, 0.0, 0.0], 1);
  assert.deepEqual(directSync, direct);

  const cypher = await engine.runCypher(
    "social",
    "CREATE (:Person {name: $name}) RETURN $name AS name",
    { name: "Ada" }
  );
  assert.deepEqual(cypher.rows, [{ name: "Ada" }]);
});

test("async errors reject the promise", async () => {
  const engine = new uqa.Engine();
  await assert.rejects(engine.sql("SELECT FROM FROM"), (error) => {
    assert.ok(error instanceof Error);
    return true;
  });
});

test("value round-trip through documents", async () => {
  const engine = new uqa.Engine();
  engine.createDefaultTable("docs", ["body"]);
  const bigValue = 2n ** 60n;
  engine.addDocument("docs", 1, {
    body: "value round trip",
    flag: true,
    missing: null,
    count: 42,
    ratio: 2.5,
    big: bigValue,
    blob: Buffer.from([1, 2, 3]),
    tags: ["a", "b"],
    nested: { inner: [1, 2] },
  });

  const doc = engine.getDocument("docs", 1);
  assert.equal(doc.body, "value round trip");
  assert.equal(doc.flag, true);
  assert.equal(doc.missing, null);
  assert.equal(doc.count, 42);
  assert.equal(doc.ratio, 2.5);
  assert.equal(doc.big, bigValue);
  assert.ok(Buffer.isBuffer(doc.blob));
  assert.deepEqual([...doc.blob], [1, 2, 3]);
  assert.deepEqual(doc.tags, ["a", "b"]);
  assert.deepEqual(doc.nested, { inner: [1, 2] });

  assert.equal(engine.documentCount("docs"), 1);
  engine.deleteDocument("docs", 1);
  assert.equal(engine.getDocument("docs", 1), null);
});

test("persistent open, batch, and format detection", async () => {
  const dir = mkdtempSync(join(tmpdir(), "uqa-node-"));

  const plain = join(dir, "plain.db");
  assert.equal(uqa.detectDatabaseFile(plain), "missing");
  const engine = uqa.open(plain);
  const results = await engine.sqlBatch([
    ["CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", []],
    ["INSERT INTO docs (id, body) VALUES ($1, $2)", [1, "hello"]],
    ["SELECT body FROM docs WHERE id = $1", [1]],
  ]);
  assert.deepEqual(results.at(-1).rows, [{ body: "hello" }]);
  engine.close();
  assert.equal(uqa.detectDatabaseFile(plain), "sqlite");

  const reopened = uqa.openAuto(plain);
  const count = reopened.sqlSync("SELECT count(*) AS n FROM docs");
  assert.deepEqual(count.rows, [{ n: 1 }]);
  reopened.close();

  const compressed = join(dir, "compressed.db");
  const compressedEngine = uqa.openCompressed(compressed);
  compressedEngine.sqlSync("CREATE TABLE t (id INTEGER PRIMARY KEY)");
  compressedEngine.close();
  assert.equal(uqa.detectDatabaseFile(compressed), "compressed");
  const compressedReopened = uqa.openAuto(compressed);
  assert.deepEqual(
    compressedReopened.sqlSync("SELECT count(*) AS n FROM t").rows,
    [{ n: 0 }]
  );
  compressedReopened.close();
});

test("close releases persistent files and is idempotent", () => {
  const dir = mkdtempSync(join(tmpdir(), "uqa-node-close-"));
  const path = join(dir, "close.db");
  const engine = uqa.open(path);
  engine.sqlSync("CREATE TABLE docs (id INTEGER PRIMARY KEY)");

  engine.close();
  engine.close();

  assert.throws(() => engine.sqlSync("SELECT 1"), /engine is closed/);
  rmSync(dir, { recursive: true });
});

test("scoring params calibration workflow", async () => {
  const engine = new uqa.Engine();
  engine.createDefaultTable("docs", ["body"]);
  const corpus = [
    "rust query engine with calibrated scoring",
    "typescript bindings for the rust engine",
    "vector search and text fusion",
    "probability calibrated hybrid retrieval",
    "postgresql compatible sql surface",
    "graph queries over the same storage",
  ];
  corpus.forEach((body, index) => {
    engine.addDocument("docs", index + 1, { body });
  });

  const params = await engine.estimateScoringParams("docs", "body", 8, 2, 42);
  assert.ok("alpha" in params && "beta" in params && "base_rate" in params);
  assert.deepEqual(engine.loadScoringParams("docs.body"), params);
  assert.deepEqual(engine.loadAllScoringParams()["docs.body"], params);

  const calibrated = await engine.search("docs", "body", "rust engine", 5, "bayesian");
  assert.ok(calibrated.length > 0);
  for (const hit of calibrated) {
    assert.ok(hit.score >= 0.0 && hit.score <= 1.0);
  }

  const labels = corpus.map((body) => (body.includes("rust") ? 1 : 0));
  const report = await engine.calibrationReport("docs", "body", "rust engine", labels);
  assert.ok(report.bins.length > 0);
  for (const key of ["ece", "brier", "logLoss"]) {
    assert.equal(typeof report[key], "number");
  }

  const learned = await engine.learnScoringParams("docs", "body", "rust engine", labels);
  assert.ok("alpha" in learned && "beta" in learned);

  const raw = await engine.search("docs", "body", "rust engine", 1, "bm25");
  engine.updateScoringParams("docs", "body", raw[0].score, 1);
  assert.throws(() => engine.updateScoringParams("docs", "body", raw[0].score, 2), /label/);

  assert.equal(engine.dropScoringParams("docs.body"), true);
  assert.equal(engine.dropScoringParams("docs.body"), false);

  // Hand-written parameters drive bayesian search: an extreme beta
  // pushes every posterior to ~0, which the identity calibration
  // (alpha=1, beta=0) could never produce for matching documents.
  engine.saveScoringParams("docs.body", { alpha: 2.0, beta: 1000.0, base_rate: 0.5 });
  const suppressed = engine.searchSync("docs", "body", "rust engine", 5, "bayesian");
  assert.ok(suppressed.length > 0);
  for (const hit of suppressed) {
    assert.ok(hit.score < 0.01);
  }
});

test("hybrid search fuses text and vector signals", async () => {
  const engine = new uqa.Engine();
  engine.createDefaultTable("docs", ["body"]);
  engine.createVectorField("docs", "embedding", 3);
  engine.addDocumentWithVectors("docs", 1, { body: "rust engine" }, { embedding: [1, 0, 0] });
  engine.addDocumentWithVectors("docs", 2, { body: "node binding" }, { embedding: [0, 1, 0] });

  const hits = await engine.hybridSearch("docs", "body", "rust", "embedding", [1, 0, 0], 2);
  assert.ok(hits.length > 0);
  assert.equal(hits[0].docId, 1);
});

test("sql notices and function depth limit", async () => {
  const engine = new uqa.Engine();
  await engine.sql("DO $$ BEGIN RAISE NOTICE 'v=% w=%% x=%', 1, 'two'; END $$");
  await engine.sql("DO $$ BEGIN RAISE WARNING 'careful'; END $$");
  assert.deepEqual(engine.takeSQLNotices(), [
    { level: "NOTICE", message: "v=1 w=% x=two" },
    { level: "WARNING", message: "careful" },
  ]);
  assert.deepEqual(engine.takeSQLNotices(), []);

  await engine.sql(`
    CREATE FUNCTION rec(n integer) RETURNS integer AS $$
    BEGIN
      IF n <= 0 THEN
        RETURN 0;
      END IF;
      RETURN rec(n - 1);
    END;
    $$ LANGUAGE plpgsql
  `);
  assert.ok(engine.sqlFunctionDepthLimit() >= 1);
  engine.setSQLFunctionDepthLimit(3);
  assert.equal(engine.sqlFunctionDepthLimit(), 3);
  await assert.rejects(engine.sql("SELECT rec(10) AS v"), /stack depth limit exceeded/);
  engine.setSQLFunctionDepthLimit(64);
  assert.deepEqual((await engine.sql("SELECT rec(10) AS v")).rows, [{ v: 0 }]);
  engine.setSQLFunctionDepthLimit(0);
  assert.equal(engine.sqlFunctionDepthLimit(), 1);
});

test("JavaScript scalar, table, and aggregate SQL callbacks", async () => {
  const engine = new uqa.Engine();
  engine.sqlSync("CREATE TABLE samples (grp TEXT, val INTEGER)");
  engine.sqlSync("INSERT INTO samples (grp, val) VALUES ('a', 1), ('a', 2), ('b', 3)");

  engine.registerScalarFunction(
    "js_prefix",
    (value) => `tag:${value}`,
    { volatility: "immutable", mayMutateEngine: false }
  );
  assert.deepEqual(engine.sqlSync("SELECT js_prefix('sync') AS tagged").rows, [
    { tagged: "tag:sync" },
  ]);
  assert.deepEqual((await engine.sql("SELECT js_prefix(grp) AS tagged FROM samples WHERE val = 3")).rows, [
    { tagged: "tag:b" },
  ]);

  engine.registerTableFunction("js_repeat_rows", (label, times) => ({
    columns: ["label", "idx"],
    rows: Array.from({ length: times }, (_, idx) => [label, idx]),
  }));
  assert.deepEqual(
    (
      await engine.sql(
        "SELECT label, idx FROM js_repeat_rows('row', 3) AS r(label, idx) ORDER BY idx"
      )
    ).rows,
    [
      { idx: 0, label: "row" },
      { idx: 1, label: "row" },
      { idx: 2, label: "row" },
    ]
  );

  engine.registerTableFunction("js_object_rows", () => [
    { name: "first", score: 2 },
    { name: "second", score: 1 },
  ]);
  assert.deepEqual(
    engine.sqlSync("SELECT name, score FROM js_object_rows() ORDER BY score").rows,
    [
      { name: "second", score: 1 },
      { name: "first", score: 2 },
    ]
  );

  engine.registerTableFunction("js_pair_rows", () => [
    ["label", "idx"],
    [["pair", 0]],
  ]);
  assert.deepEqual(
    engine.sqlSync("SELECT label, idx FROM js_pair_rows() AS r(label, idx)").rows,
    [{ idx: 0, label: "pair" }]
  );

  engine.registerAggregateFunction("js_sum_squares", () => ({
    total: 0,
    observe(value) {
      if (value !== null) {
        this.total += value * value;
      }
    },
    finish() {
      return this.total;
    },
  }));
  assert.deepEqual(
    (
      await engine.sql(
        "SELECT grp, js_sum_squares(val) AS total FROM samples GROUP BY grp ORDER BY grp"
      )
    ).rows,
    [
      { grp: "a", total: 5 },
      { grp: "b", total: 9 },
    ]
  );

  engine.registerAggregateFunction("js_sum_aliases", () => ({
    total: 0,
    step(value) {
      this.total += value;
    },
    finalize() {
      return this.total;
    },
  }));
  assert.deepEqual(engine.sqlSync("SELECT js_sum_aliases(val) AS total FROM samples").rows, [
    { total: 6 },
  ]);
});

test("JavaScript SQL callbacks are shared by sessions and propagate errors", async () => {
  const dir = mkdtempSync(join(tmpdir(), "uqa-node-callbacks-"));
  const engine = uqa.open(join(dir, "callbacks.db"));
  engine.registerScalarFunction("js_double", (value) => value * 2);
  const session = engine.newSession();

  const [parent, child] = await Promise.all([
    engine.sql("SELECT js_double(4) AS value"),
    session.sql("SELECT js_double(5) AS value"),
  ]);
  assert.deepEqual(parent.rows, [{ value: 8 }]);
  assert.deepEqual(child.rows, [{ value: 10 }]);

  session.registerScalarFunction("js_double", (value) => value * 3);
  assert.deepEqual(engine.sqlSync("SELECT js_double(4) AS value").rows, [{ value: 12 }]);

  engine.registerScalarFunction("js_throws", () => {
    throw new Error("callback exploded");
  });
  assert.throws(() => engine.sqlSync("SELECT js_throws()"), /callback exploded/);
  await assert.rejects(engine.sql("SELECT js_throws()"), /callback exploded/);

  engine.registerScalarFunction("js_promise", async () => 1);
  assert.throws(() => engine.sqlSync("SELECT js_promise()"), /must return synchronously/);
  await assert.rejects(engine.sql("SELECT js_promise()"), /must return synchronously/);

  engine.registerTableFunction("js_table_promise", async () => []);
  await assert.rejects(engine.sql("SELECT * FROM js_table_promise()"), /must return synchronously/);

  engine.registerAggregateFunction("js_factory_promise", async () => ({
    observe() {},
    finish() {
      return 1;
    },
  }));
  await assert.rejects(
    engine.sql("SELECT js_factory_promise(val) AS value FROM (VALUES (1)) AS t(val)"),
    /must return synchronously/
  );

  engine.registerScalarFunction("js_reenter", () => engine.sqlSync("SELECT 1"));
  assert.throws(
    () => engine.sqlSync("SELECT js_reenter()"),
    /Engine methods cannot be called from a JavaScript SQL callback/
  );
  await assert.rejects(
    engine.sql("SELECT js_reenter()"),
    /Engine methods cannot be called from a JavaScript SQL callback/
  );

  assert.throws(
    () =>
      engine.registerScalarFunction("invalid_options", (value) => value, {
        volatility: "stable",
      }),
    /must be VOLATILE/i
  );

  session.close();
  engine.close();
});
