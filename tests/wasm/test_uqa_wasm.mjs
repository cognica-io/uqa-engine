//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Exercises the browser WASM package through its TypeScript-facing
// wrapper. Node has no IndexedDB, so persistence here runs on the
// module's in-memory filesystem; the IDBFS mount only activates in
// browsers.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  Engine,
  HttpEngine,
  HttpEngineError,
  SQLParam,
  UQA,
  vector,
  tensor,
} from "../../crates/uqa-wasm/js/index.mjs";

test("HTTP engine executes SQL, atomic batches, and streams", async (context) => {
  const originalFetch = globalThis.fetch;
  context.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async (url, options) => {
    assert.equal(options.headers.authorization, "Bearer uqa_db_test");
    assert.equal(options.credentials, "omit");
    assert.equal(options.redirect, "error");
    assert.equal(options.referrerPolicy, "no-referrer");
    const request = JSON.parse(options.body);
    if (url.pathname === "/v1/sql") {
      if (request.sql === "SELECT transport_error") {
        const body = new ReadableStream({
          start(controller) {
            controller.error(new Error("secret upstream transport diagnostic"));
          },
        });
        return new Response(body, {
          headers: {
            "content-type": "application/json",
            "x-request-id": "qry_browser_transport",
          },
        });
      }
      if (request.sql === "SELECT unsafe") {
        return new Response(JSON.stringify({
          columns: ["unsafe"],
          rows: [{ unsafe: 9_007_199_254_740_992 }],
          affected_rows: 0,
          request_id: "qry_browser_unsafe",
        }), {
          headers: {
            "content-type": "application/json",
            "x-request-id": "qry_browser_unsafe",
          },
        });
      }
      if (request.sql === "SELECT special") {
        assert.deepEqual(request.params[0], { type: "bytes", hex: "00ff" });
        assert.equal(request.params[1].type, "json");
        assert.equal(Object.hasOwn(request.params[1].value, "__proto__"), true);
        assert.equal(request.params[1].value.__proto__, "preserved");
        assert.deepEqual(request.params[2], {
          type: "json",
          value: { $bytes: "ordinary-json", $vector: [1, 2] },
        });
        return new Response(JSON.stringify({
          columns: [
            "$uqa_type",
            "hex",
            "document",
            "created_on",
            "duration",
            "tag_with_extra",
            "invalid_array",
            "matrix",
            "outside_chrono",
          ],
          rows: [{
            $uqa_type: "bytes",
            hex: "column",
            document: {
              $uqa_type: "json",
              value: "{\"$uqa_type\":\"bytes\",\"hex\":\"00ff\",\"__proto__\":\"preserved\"}",
            },
            created_on: { $uqa_type: "date", days: 2_932_897 },
            duration: {
              $uqa_type: "interval",
              months: -1,
              days: 1,
              micros: 3_600_000_000,
            },
            tag_with_extra: {
              $uqa_type: "bytes",
              hex: "00ff",
              extra: true,
            },
            invalid_array: {
              $uqa_type: "array",
              lower_bounds: [1],
              values: [[1, 2], [3, 4]],
            },
            matrix: {
              $uqa_type: "array",
              lower_bounds: [0, -2],
              values: [[1, 2], [3, 4]],
            },
            outside_chrono: { $uqa_type: "date", days: 100_000_000 },
          }],
          affected_rows: 0,
          request_id: "qry_browser_special",
        }), {
          headers: {
            "content-type": "application/json",
            "x-request-id": "qry_browser_special",
          },
        });
      }
      assert.deepEqual(request.params, [
        { type: "int64", value: 7 },
        { type: "vector", value: [0.25, 0.75] },
      ]);
      return new Response(JSON.stringify({
        columns: ["answer", "payload"],
        rows: [{ answer: 7, payload: { $uqa_type: "bytes", hex: "00ff" } }],
        affected_rows: 0,
        request_id: "qry_browser",
      }), {
        headers: {
          "content-type": "application/json",
          "x-request-id": "qry_browser",
        },
      });
    }
    if (url.pathname === "/v1/sql/batch") {
      assert.equal(request.statements.length, 2);
      return new Response(JSON.stringify({
        results: [
          { columns: [], rows: [], affected_rows: 1 },
          { columns: ["n"], rows: [{ n: 1 }], affected_rows: 0 },
        ],
        request_id: "qry_browser_batch",
      }), {
        headers: {
          "content-type": "application/json",
          "x-request-id": "qry_browser_batch",
        },
      });
    }
    if (url.pathname === "/v1/sql/stream") {
      const frames = [
        {
          type: "metadata",
          columns: ["n"],
          row_count: 1,
          spilled_to_disk: false,
          request_id: "qry_browser_stream",
        },
        { type: "row", row: { n: 1 } },
        { type: "complete", row_count: 1, request_id: "qry_browser_stream" },
      ];
      if (request.sql === "SELECT trailing") {
        frames.push({ type: "row", row: { n: 2 } });
      }
      const body = frames.map((frame) => `${JSON.stringify(frame)}\n`).join("");
      return new Response(body, {
        headers: {
          "content-type": "application/x-ndjson",
          "x-request-id": "qry_browser_stream",
        },
      });
    }
    throw new Error(`unexpected test URL ${url.pathname}`);
  };

  const engine = new HttpEngine("http://127.0.0.1:8432/", "uqa_db_test");
  assert.equal(JSON.stringify(engine), "{}");
  assert.throws(
    () => new HttpEngine("http://example.com/", "uqa_db_secret"),
    /plain HTTP UQA URLs must resolve to loopback/,
  );
  const execution = await engine.sqlWithMetadata("SELECT $1", [
    7,
    SQLParam.vector([0.25, 0.75]),
  ]);
  assert.equal(execution.requestId, "qry_browser");
  assert.deepEqual([...execution.result.rows[0].payload], [0, 255]);
  await assert.rejects(engine.sql("SELECT unsafe"), /browser safe range/);

  const special = await engine.sql("SELECT special", [
    SQLParam.scalar(new Uint8Array([0, 255])),
    JSON.parse('{"__proto__":"preserved"}'),
    SQLParam.scalar({ $bytes: "ordinary-json", $vector: [1, 2] }),
  ]);
  assert.equal(special.rows[0].created_on, "+10000-01-01");
  assert.equal(special.rows[0].duration, "-1 mons +1 day +01:00:00");
  assert.equal(special.rows[0].$uqa_type, "bytes");
  assert.equal(special.rows[0].hex, "column");
  assert.equal(special.rows[0].document.$uqa_type, "bytes");
  assert.equal(special.rows[0].document.hex, "00ff");
  assert.equal(Object.hasOwn(special.rows[0].document, "__proto__"), true);
  assert.equal(special.rows[0].document.__proto__, "preserved");
  assert.deepEqual(special.rows[0].tag_with_extra, {
    $uqa_type: "bytes",
    hex: "00ff",
    extra: true,
  });
  assert.deepEqual(special.rows[0].invalid_array, {
    $uqa_type: "array",
    lower_bounds: [1],
    values: [[1, 2], [3, 4]],
  });
  assert.deepEqual(special.rows[0].matrix, [[1, 2], [3, 4]]);
  assert.equal(special.rows[0].outside_chrono, "100000000");

  await assert.rejects(engine.sql("SELECT transport_error"), (error) => {
    assert.ok(error instanceof HttpEngineError);
    assert.equal(error.message, "UQA HTTP transport failed");
    assert.doesNotMatch(error.message, /secret/);
    return true;
  });

  const batch = await engine.sqlBatchWithMetadata([
    ["INSERT INTO t VALUES (1)", []],
    ["SELECT 1 AS n", []],
  ]);
  assert.equal(batch.requestId, "qry_browser_batch");
  assert.deepEqual(batch.results.at(-1).rows, [{ n: 1 }]);

  const stream = await engine.sqlStream("SELECT 1 AS n");
  assert.equal(stream.requestId, "qry_browser_stream");
  const frames = [];
  for await (const frame of stream) {
    frames.push(frame);
  }
  assert.deepEqual(frames.map((frame) => frame.type), ["metadata", "row", "complete"]);
  assert.deepEqual(frames[1].row, { n: 1 });

  const trailing = await engine.sqlStream("SELECT trailing");
  assert.equal((await trailing.nextFrame()).type, "metadata");
  assert.equal((await trailing.nextFrame()).type, "row");
  assert.equal((await trailing.nextFrame()).type, "complete");
  await assert.rejects(trailing.nextFrame(), /frame order is invalid/);
});

test("sql, params, vector, tensor, and cypher surfaces", async () => {
  const engine = await Engine.inMemory();
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
      vector([1.0, 0.0, 0.0]),
      tensor([
        [1.0, 0.0, 0.0],
        [0.8, 0.1, 0.0],
      ]),
    ]
  );
  await engine.sql(
    "INSERT INTO notes (id, title, body, embedding, chunks) VALUES ($1, $2, $3, $4, $5)",
    [
      2,
      "browser client",
      "wasm package binding",
      SQLParam.vector(new Float32Array([0.0, 1.0, 0.0])),
      SQLParam.tensor([[0.0, 1.0, 0.0]]),
    ]
  );
  await engine.sql("CREATE INDEX notes_body_idx ON notes USING gin (body)");

  const text = await engine.sql(
    "SELECT id, _score FROM notes WHERE text_match(body, 'rust') ORDER BY _score DESC"
  );
  assert.equal(text.rows[0].id, 1);

  const direct = await engine.knnSearch("notes", "embedding", new Float32Array([1, 0, 0]), 1);
  assert.equal(direct[0].docId, 1);

  const cypher = await engine.runCypher(
    "social",
    "CREATE (:Person {name: $name}) RETURN $name AS name",
    { name: "Ada" }
  );
  assert.deepEqual(cypher.rows, [{ name: "Ada" }]);

  await assert.rejects(engine.sql("SELECT FROM FROM"), /error|syntax/i);
});

test("value round-trip through documents", async () => {
  const engine = await Engine.inMemory();
  await engine.createDefaultTable("docs", ["body"]);
  await engine.addDocument("docs", 1, {
    body: "value round trip",
    flag: true,
    missing: null,
    count: 42,
    ratio: 2.5,
    blob: new Uint8Array([1, 2, 3]),
    tags: ["a", "b"],
    nested: { inner: [1, 2] },
  });

  const doc = await engine.getDocument("docs", 1);
  assert.equal(doc.body, "value round trip");
  assert.equal(doc.flag, true);
  assert.equal(doc.missing, null);
  assert.equal(doc.count, 42);
  assert.equal(doc.ratio, 2.5);
  assert.ok(doc.blob instanceof Uint8Array);
  assert.deepEqual([...doc.blob], [1, 2, 3]);
  assert.deepEqual(doc.tags, ["a", "b"]);
  assert.deepEqual(doc.nested, { inner: [1, 2] });

  assert.equal(await engine.documentCount("docs"), 1);
  await engine.deleteDocument("docs", 1);
  assert.equal(await engine.getDocument("docs", 1), null);
});

test("persistent open, format detection, and encryption rejection", async () => {
  const path = `${UQA.persistDir}/test.db`;
  assert.equal(await UQA.detectDatabaseFile(path), "missing");

  const engine = await Engine.open(path);
  const results = await engine.sqlBatch([
    ["CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", []],
    ["INSERT INTO docs (id, body) VALUES ($1, $2)", [1, "hello"]],
    ["SELECT body FROM docs WHERE id = $1", [1]],
  ]);
  assert.deepEqual(results.at(-1).rows, [{ body: "hello" }]);
  await engine.close();
  assert.equal(await UQA.detectDatabaseFile(path), "sqlite");

  const reopened = await Engine.openAuto(path);
  assert.deepEqual((await reopened.sql("SELECT count(*) AS n FROM docs")).rows, [{ n: 1 }]);
  await reopened.close();

  await assert.rejects(UQA.persist(), /IndexedDB.*unavailable/i);

  const compressed = `${UQA.persistDir}/compressed.db`;
  const compressedEngine = await Engine.openCompressed(compressed);
  await compressedEngine.sql("CREATE TABLE t (id INTEGER PRIMARY KEY)");
  await compressedEngine.close();
  assert.equal(await UQA.detectDatabaseFile(compressed), "compressed");
});

test("scoring params calibration workflow", async () => {
  const engine = await Engine.inMemory();
  await engine.createDefaultTable("docs", ["body"]);
  const corpus = [
    "rust query engine with calibrated scoring",
    "browser bindings for the rust engine",
    "vector search and text fusion",
    "probability calibrated hybrid retrieval",
    "postgresql compatible sql surface",
    "graph queries over the same storage",
  ];
  for (const [index, body] of corpus.entries()) {
    await engine.addDocument("docs", index + 1, { body });
  }

  const params = await engine.estimateScoringParams("docs", "body", 8, 2, 42);
  assert.ok("alpha" in params && "beta" in params && "base_rate" in params);
  assert.deepEqual(await engine.loadScoringParams("docs.body"), params);
  assert.deepEqual((await engine.loadAllScoringParams())["docs.body"], params);

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

  assert.equal(await engine.dropScoringParams("docs.body"), true);
  assert.equal(await engine.dropScoringParams("docs.body"), false);

  await engine.saveScoringParams("docs.body", { alpha: 2.0, beta: 1000.0, base_rate: 0.5 });
  const suppressed = await engine.search("docs", "body", "rust engine", 5, "bayesian");
  assert.ok(suppressed.length > 0);
  for (const hit of suppressed) {
    assert.ok(hit.score < 0.01);
  }
});

test("hybrid search fuses text and vector signals", async () => {
  const engine = await Engine.inMemory();
  await engine.createDefaultTable("docs", ["body"]);
  await engine.createVectorField("docs", "embedding", 3);
  await engine.addDocumentWithVectors("docs", 1, { body: "rust engine" }, { embedding: [1, 0, 0] });
  await engine.addDocumentWithVectors("docs", 2, { body: "wasm binding" }, { embedding: [0, 1, 0] });

  const hits = await engine.hybridSearch("docs", "body", "rust", "embedding", [1, 0, 0], 2);
  assert.ok(hits.length > 0);
  assert.equal(hits[0].docId, 1);
});

test("sql notices, depth limit, and encryption rejection", async () => {
  const engine = await Engine.inMemory();
  await engine.sql("DO $$ BEGIN RAISE NOTICE 'v=% w=%% x=%', 1, 'two'; END $$");
  await engine.sql("DO $$ BEGIN RAISE WARNING 'careful'; END $$");
  assert.deepEqual(await engine.takeSQLNotices(), [
    { level: "NOTICE", message: "v=1 w=% x=two" },
    { level: "WARNING", message: "careful" },
  ]);
  assert.deepEqual(await engine.takeSQLNotices(), []);

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
  await engine.setSQLFunctionDepthLimit(3);
  assert.equal(await engine.sqlFunctionDepthLimit(), 3);
  await assert.rejects(engine.sql("SELECT rec(10) AS v"), /stack depth limit exceeded/);
  await engine.setSQLFunctionDepthLimit(64);
  assert.deepEqual((await engine.sql("SELECT rec(10) AS v")).rows, [{ v: 0 }]);
});

test("browser scalar, table, and aggregate SQL callbacks", async () => {
  const engine = await Engine.inMemory();
  await engine.sql("CREATE TABLE samples (grp TEXT, val INTEGER)");
  await engine.sql("INSERT INTO samples (grp, val) VALUES ('a', 1), ('a', 2), ('b', 3)");

  await engine.registerScalarFunction(
    "js_prefix",
    (value) => `tag:${value}`,
    { volatility: "immutable", mayMutateEngine: false }
  );
  assert.deepEqual((await engine.sql("SELECT js_prefix(grp) AS tagged FROM samples WHERE val = 3")).rows, [
    { tagged: "tag:b" },
  ]);

  await engine.registerTableFunction("js_repeat_rows", (label, times) => ({
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

  await engine.registerTableFunction("js_object_rows", () => [
    { name: "first", score: 2 },
    { name: "second", score: 1 },
  ]);
  assert.deepEqual((await engine.sql("SELECT name, score FROM js_object_rows() ORDER BY score")).rows, [
    { name: "second", score: 1 },
    { name: "first", score: 2 },
  ]);

  await engine.registerTableFunction("js_pair_rows", () => [
    ["label", "idx"],
    [["pair", 0]],
  ]);
  assert.deepEqual(
    (await engine.sql("SELECT label, idx FROM js_pair_rows() AS r(label, idx)")).rows,
    [{ idx: 0, label: "pair" }]
  );

  await engine.registerAggregateFunction("js_sum_squares", () => ({
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

  await engine.registerAggregateFunction("js_sum_aliases", () => ({
    total: 0,
    step(value) {
      this.total += value;
    },
    finalize() {
      return this.total;
    },
  }));
  assert.deepEqual((await engine.sql("SELECT js_sum_aliases(val) AS total FROM samples")).rows, [
    { total: 6 },
  ]);

  await engine.close();
});

test("browser SQL callbacks are shared by sessions and propagate errors", async () => {
  const engine = await Engine.open(`${UQA.persistDir}/callback-sessions.db`);
  await engine.registerScalarFunction("js_double", (value) => value * 2);
  const session = await engine.newSession();
  assert.deepEqual((await session.sql("SELECT js_double(5) AS value")).rows, [{ value: 10 }]);

  await session.registerScalarFunction("js_double", (value) => value * 3);
  assert.deepEqual((await engine.sql("SELECT js_double(4) AS value")).rows, [{ value: 12 }]);

  await engine.registerScalarFunction("js_throws", () => {
    throw new Error("callback exploded");
  });
  await assert.rejects(engine.sql("SELECT js_throws()"), /callback exploded/);

  await engine.registerScalarFunction("js_promise", async () => 1);
  await assert.rejects(engine.sql("SELECT js_promise()"), /must return synchronously/);

  await engine.registerTableFunction("js_table_promise", async () => []);
  await assert.rejects(engine.sql("SELECT * FROM js_table_promise()"), /must return synchronously/);

  await engine.registerAggregateFunction("js_factory_promise", async () => ({
    observe() {},
    finish() {
      return 1;
    },
  }));
  await assert.rejects(
    engine.sql("SELECT js_factory_promise(val) AS value FROM (VALUES (1)) AS t(val)"),
    /must return synchronously/
  );

  await engine.registerScalarFunction("js_reenter", () => engine.sql("SELECT 1"));
  await assert.rejects(
    engine.sql("SELECT js_reenter()"),
    /Engine methods cannot be called from a JavaScript SQL callback/
  );

  await assert.rejects(
    engine.registerScalarFunction("invalid_options", (value) => value, {
      volatility: "stable",
    }),
    /must be VOLATILE/i
  );

  await session.close();
  assert.deepEqual((await engine.sql("SELECT js_double(6) AS value")).rows, [{ value: 18 }]);
  await engine.close();
});
