//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Run against an isolated, authenticated SQL HTTP server supplied by its integration harness.
import assert from "node:assert/strict";
import { HttpEngine, HttpEngineError, vector } from "../../crates/uqa-node/http.js";

const engine = new HttpEngine(process.env.UQA_HTTP_TEST_URL, process.env.UQA_HTTP_TEST_TOKEN);
await engine.sql("CREATE TABLE js_diagnostic_private (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(3))");
await engine.sql("INSERT INTO js_diagnostic_private VALUES (0, 'js_diagnostic_private', $1)", [vector([1,0,0])]);
for (const [sql, state, category, position] of [
  ["SELECT * FROM missing_js_diagnostic_private", "42P01", "undefined_table"],
  ["SELECT missing_js_diagnostic_private FROM js_diagnostic_private", "42703", "undefined_column"],
  ["SELECT ) /* js_diagnostic_private */", "42601", "syntax", 8],
  ["INSERT INTO js_diagnostic_private VALUES (2, 'js_diagnostic_private', $1)", "22023", "vector_dimension_mismatch"],
  ["SELECT id FROM js_diagnostic_private WHERE text_match(body, 'js_diagnostic_private')", "42804", "index_required"],
]) {
  const params = category === "vector_dimension_mismatch" ? [vector([1,0])] : [];
  await assert.rejects(engine.sql(sql, params), (error) => {
    assert.ok(error instanceof HttpEngineError);
    assert.equal(error.diagnostic.sqlstate, state);
    assert.equal(error.diagnostic.category, category);
    assert.equal(error.diagnostic.position, position);
    assert.ok(!error.message.includes("js_diagnostic_private"));
    return true;
  });
  if (sql.startsWith("SELECT")) {
    if (category === "syntax") {
      await assert.rejects(engine.sqlStream(sql), (error) => {
        assert.ok(error instanceof HttpEngineError);
        assert.equal(error.status, 400);
        assert.equal(error.code, "SQL_EXECUTION_FAILED");
        assert.equal(error.diagnostic.sqlstate, state);
        assert.equal(error.diagnostic.category, category);
        assert.equal(error.diagnostic.position, position);
        assert.ok(!error.message.includes("js_diagnostic_private"));
        return true;
      });
      continue;
    }
    const stream = await engine.sqlStream(sql);
    const frame = await stream.nextFrame();
    assert.equal(frame.type, "error");
    assert.equal(frame.diagnostic.sqlstate, state);
    assert.equal(frame.diagnostic.category, category);
    assert.ok(!frame.message.includes("js_diagnostic_private"));
    assert.equal(await stream.nextFrame(), null);
  }
}
await assert.rejects(engine.sqlBatch([
  ["INSERT INTO js_diagnostic_private (id) VALUES (1)", []],
  ["SELECT missing_js_diagnostic_private FROM js_diagnostic_private", []],
  ["INSERT INTO js_diagnostic_private (id) VALUES (3)", []],
]), (error) => {
  assert.equal(error.diagnostic.statementIndex, 1);
  assert.equal(error.diagnostic.sqlstate, "42703");
  return true;
});
assert.deepEqual((await engine.sql("SELECT id FROM js_diagnostic_private ORDER BY id")).rows, [{ id: 0 }]);
console.log("Node.js HTTP SQL diagnostics: PASS");
