//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

const assert = require("node:assert/strict");
const { basename } = require("node:path");

assert.equal(process.env.UQA_TOKEN, undefined);
if (process.env.UQA_CLI_FIXTURE_MODE === "native") {
  const expected = basename(process.argv[1]) === "local"
    ? ["connection", "notes", "--format", "json"]
    : ["connection", "analytics", "--format", "json", "--org", "acme"];
  assert.deepEqual(process.argv.slice(2), expected);
  process.stdout.write(JSON.stringify({ url: process.env.UQA_CLI_FIXTURE_ORIGIN, token: "uqa_db_test" }));
} else if (basename(process.argv[1]) === "cloud") {
  assert.deepEqual(process.argv.slice(2), ["connection", "a;$(false)", "--format", "json", "--org", "organization"]);
  process.stdout.write(JSON.stringify({ url: process.env.UQA_CLI_FIXTURE_ORIGIN, token: "token" }));
} else {
  assert.equal(basename(process.argv[1]), "local");
  assert.deepEqual(process.argv.slice(2), ["connection", "project", "--format", "json"]);
  switch (process.env.UQA_CLI_FIXTURE_MODE) {
    case "exit": process.stderr.write("private token"); process.exitCode = 1; break;
    case "stdout-limit": process.stdout.write("private token".repeat(10000)); break;
    case "stderr-limit": process.stderr.write("private token".repeat(10000)); break;
    case "invalid": process.stdout.write("private malformed JSON"); break;
    default: throw new Error("unknown CLI fixture mode");
  }
}
