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

import { Engine, SQLParam, UQA, vector, tensor } from "../../crates/uqa-wasm/js/index.mjs";

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

  // persist() is a no-op outside the browser but must not throw.
  await UQA.persist();

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
