//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

const assert = require("node:assert/strict");
const { inspect } = require("node:util");
const [packagePath, mode, origin] = process.argv.slice(2);
const { HttpEngine, HttpEngineError } = require(packagePath);

async function run() {
  // The real resolver starts the existing Node executable. Its first CLI argument
  // selects a checked-in script in this private child's cwd; no executable is
  // created or rewritten, and parent process state is never changed.
  const cliPath = process.execPath;
  assert.equal(process.env.UQA_TOKEN, "must-not-reach-child");
  process.env.UQA_CLI_FIXTURE_ORIGIN = origin;
  if (mode === "cloud") {
    const engine = await HttpEngine.cloud("a;$(false)", { cliPath, organization: "organization" });
    assert.deepEqual((await engine.sql("SELECT 1")).rows, [{ n: 1 }]);
  } else if (mode === "native") {
    process.env.UQA_CLI_FIXTURE_MODE = mode;
    const local = await HttpEngine.local("notes", { cliPath });
    assert.equal((await local.sql("SELECT $1", [7])).rows[0].answer, 7);
    const cloud = await HttpEngine.cloud("analytics", { cliPath, organization: "acme" });
    assert.equal((await cloud.sql("SELECT $1", [7])).rows[0].answer, 7);
  } else {
    assert.equal(mode, "failures");
    for (const [fixture, message] of [
      ["exit", "UQA CLI connection command failed"],
      ["stdout-limit", "UQA CLI connection output exceeded the client safety limit"],
      ["stderr-limit", "UQA CLI connection output exceeded the client safety limit"],
      ["invalid", "UQA CLI connection output is invalid"],
    ]) {
      process.env.UQA_CLI_FIXTURE_MODE = fixture;
      await assert.rejects(HttpEngine.local("project", { cliPath }), (error) => {
        assert.ok(error instanceof HttpEngineError);
        assert.equal(error.message, message);
        for (const secret of ["private token", "private malformed JSON"]) {
          assert.equal(inspect(error).includes(secret), false);
        }
        return true;
      });
    }
  }
  assert.equal(process.env.UQA_TOKEN, "must-not-reach-child");
}

run().catch((error) => { console.error(error); process.exitCode = 1; });
