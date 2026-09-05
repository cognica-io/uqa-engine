//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test, after } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, chmodSync, rmSync, readdirSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { execFile } from "node:child_process";
import { promisify, inspect } from "node:util";

const exec = promisify(execFile);
const source = fileURLToPath(new URL("../../crates/uqa-node/", import.meta.url));
const directory = mkdtempSync(join(tmpdir(), "uqa-node-http-"));
const packagePath = join(directory, "node_modules", "@cognica-io", "uqa");
const manifest = JSON.parse(readFileSync(join(source, "package.json"), "utf8"));
mkdirSync(packagePath, { recursive: true });
for (const file of ["package.json", ...manifest.files]) {
  mkdirSync(dirname(join(packagePath, file)), { recursive: true });
  copyFileSync(join(source, file), join(packagePath, file));
}
const require = createRequire(join(directory, "application.cjs"));
const uqa = require("@cognica-io/uqa");
after(() => rmSync(directory, { recursive: true, force: true }));

async function server(handler, run) {
  let handlerError;
  const instance = createServer((request, response) => {
    Promise.resolve(handler(request, response)).catch((error) => {
      handlerError = error;
      response.destroy();
    });
  });
  await new Promise((resolve) => instance.listen(0, "127.0.0.1", resolve));
  try {
    await run("http://127.0.0.1:" + instance.address().port);
    if (handlerError) throw handlerError;
  } finally {
    await new Promise((resolve) => instance.close(resolve));
  }
}

async function body(request) {
  const chunks = [];
  for await (const chunk of request) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

function json(response, payload, options = {}) {
  response.writeHead(options.status ?? 200, {
    "content-type": "application/json",
    "x-request-id": "request-test",
    ...options.headers,
  });
  response.end(typeof payload === "string" ? payload : JSON.stringify(payload));
}

const result = (row = { n: 1 }) => ({
  request_id: "request-test", columns: Object.keys(row), rows: [row], affected_rows: 0,
});
const metadata = { type: "metadata", columns: ["n"], row_count: 1, spilled_to_disk: false, request_id: "request-test" };
const complete = { type: "complete", row_count: 1, request_id: "request-test" };

test("package imports expose HTTP and parameters without loading a native addon", async () => {
  assert.equal(readdirSync(packagePath).some((name) => name.endsWith(".node")), false);
  const http = require("@cognica-io/uqa/http");
  const esm = await import(pathToFileURL(join(packagePath, "api.js")));
  const httpESM = await import(pathToFileURL(join(packagePath, "http.js")));
  assert.equal(esm.HttpEngine, uqa.HttpEngine);
  assert.equal(http.HttpEngine, uqa.HttpEngine);
  assert.equal(httpESM.SQLParam, uqa.SQLParam);
  uqa.SQLParam.scalar(9223372036854775807n);
  uqa.vector([1, 0]);
  uqa.tensor([[1], [2]]);
  inspect(uqa);
  JSON.stringify(uqa);
  assert.equal({} instanceof uqa.Engine, false);
  assert.equal(Object.keys(require.cache).includes(join(packagePath, "index.js")), false);
  assert.throws(() => new uqa.Engine(), /native binding/);
});

test("HTTP requests preserve int64, bytes, structured parameters, and parameter snapshots", async () => {
  const values = [0.1, 1];
  const vector = uqa.vector(values);
  values[0] = 99;
  await server(async (request, response) => {
    assert.equal(request.headers.authorization, "Bearer token-test");
    const text = await body(request);
    assert.ok(text.includes('"type":"int64","value":9223372036854775807'));
    assert.ok(text.includes('"type":"int64","value":-9223372036854775808'));
    assert.ok(text.includes('"value":[1.0,2.5]'));
    const parameters = JSON.parse(text).params;
    assert.deepEqual(parameters[2], { type: "bytes", hex: "00ff" });
    assert.deepEqual(parameters[3], { type: "vector", value: [Math.fround(0.1), 1] });
    assert.deepEqual(parameters[4], { type: "tensor", value: [[1, 2]] });
    json(response, '{"request_id":"request-test","columns":["max","min","float","bytes","text"],"affected_rows":0,"rows":[{"max":9223372036854775807,"min":-9223372036854775808,"float":100000000000000000000.0,"bytes":{"$uqa_type":"bytes","hex":"00ff"},"text":"한글 😀"}]}');
  }, async (origin) => {
    const engine = new uqa.HttpEngine(origin, "token-test");
    const output = await engine.sql("SELECT $1", [
      9223372036854775807n, -9223372036854775808n, Buffer.from([0, 255]),
      vector, uqa.tensor([[1, 2]]), new Float64Array([1, 2.5]),
    ]);
    assert.deepEqual(output.rows[0], {
      max: 9223372036854775807n, min: -9223372036854775808n, float: 1e20,
      bytes: Buffer.from([0, 255]), text: "한글 😀",
    });
    assert.equal(inspect(engine).includes("token-test"), false);
  });
});

test("HTTP results preserve tagged values, nested integers, and prototype-shaped keys", async () => {
  await server((request, response) => json(response,
    '{"request_id":"request-test","columns":["v"],"affected_rows":0,"rows":[{"v":{"$uqa_type":"json","value":"{\\"n\\":9223372036854775807,\\"$uqa_type\\":\\"bytes\\",\\"hex\\":\\"ff\\"}"},"__proto__":{"safe":true},"record":{"$uqa_type":"record","fields":[["__proto__",7]]},"timestamp":{"$uqa_type":"timestamp_tz","micros":9007199254740993},"interval":{"$uqa_type":"interval","months":-13,"days":2,"micros":9223372036854775807},"bytesMap":{"$uqa_type":"bytes","hex":"ff","extra":true},"void":{"$uqa_type":"void"}}]}'),
  async (origin) => {
    const row = (await new uqa.HttpEngine(origin, "token").sql("SELECT 1")).rows[0];
    assert.deepEqual(row.v, { n: 9223372036854775807n, $uqa_type: "bytes", hex: "ff" });
    assert.equal(Object.getPrototypeOf(row), Object.prototype);
    assert.deepEqual(row.__proto__, { safe: true });
    assert.equal(row.record.__proto__, 7);
    assert.equal(row.timestamp, "2255-06-05 23:47:34.740993+00");
    assert.equal(row.interval, "-1 years -1 mons +2 days +2562047788:00:54.775807");
    assert.deepEqual(row.bytesMap, { $uqa_type: "bytes", hex: "ff", extra: true });
    assert.equal(row.void, "");
  });
});

test("invalid parameters fail before sending any SQL", async () => {
  let requests = 0;
  await server((request, response) => { requests += 1; json(response, result()); }, async (origin) => {
    const engine = new uqa.HttpEngine(origin, "token");
    const cyclic = {}; cyclic.self = cyclic;
    for (const value of [NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 1n << 63n, -(1n << 63n) - 1n, new Date(), cyclic, { nested: Infinity }]) {
      await assert.rejects(engine.sql("SELECT $1", [value]));
    }
    await assert.rejects(engine.sql(" "));
    await assert.rejects(engine.sqlBatch([["SELECT 1", []], ["", []]]));
    assert.throws(() => uqa.vector([NaN]));
    assert.throws(() => uqa.vector([1e100]));
    assert.equal(requests, 0);
  });
});

test("environment construction and atomic batch use the direct HTTP endpoint", async () => {
  await server(async (request, response) => {
    assert.equal(request.url, "/v1/sql/batch");
    assert.deepEqual(JSON.parse(await body(request)), { statements: [
      { sql: "INSERT INTO t VALUES ($1)", params: [{ type: "text", value: "test" }] },
      { sql: "SELECT * FROM t", params: [] },
    ] });
    json(response, { request_id: "request-test", results: [
      { columns: [], rows: [], affected_rows: 1 },
      { columns: ["n"], rows: [{ n: 1 }], affected_rows: 0 },
    ] });
  }, async (origin) => {
    const engine = uqa.HttpEngine.fromEnv({ UQA_URL: origin, UQA_TOKEN: "token" });
    const output = await engine.sqlBatchWithMetadata([["INSERT INTO t VALUES ($1)", ["test"]], ["SELECT * FROM t", []]]);
    assert.equal(output.requestId, "request-test");
    assert.equal(output.results[0].affectedRows, 1);
    assert.deepEqual(output.results[1].rows, [{ n: 1 }]);
  });
});

test("NDJSON handles fragmented UTF-8, int64, CRLF, and concurrent nextFrame calls", async () => {
  await server((request, response) => {
    response.writeHead(200, { "content-type": "application/x-ndjson", "x-request-id": "request-test" });
    const text = JSON.stringify(metadata) + '\r\n{"type":"row","row":{"n":9223372036854775807,"text":"한글😀"}}\r\n' + JSON.stringify(complete);
    for (const byte of Buffer.from(text)) response.write(Buffer.from([byte]));
    response.end();
  }, async (origin) => {
    const stream = await new uqa.HttpEngine(origin, "token").sqlStream("SELECT 1");
    const frames = await Promise.all(Array.from({ length: 4 }, () => stream.nextFrame()));
    assert.deepEqual(frames.map((frame) => frame?.type ?? null), ["metadata", "row", "complete", null]);
    assert.deepEqual(frames[1].row, { n: 9223372036854775807n, text: "한글😀" });
  });
});

test("NDJSON rejects truncation, trailing frames, invalid order, and request identity", async () => {
  const transcripts = [
    [metadata, { type: "row", row: {} }],
    [metadata, complete, { type: "row", row: {} }],
    [{ type: "row", row: {} }, complete],
    [{ ...metadata, request_id: "other" }, complete],
    [metadata, { ...complete, request_id: undefined }],
    [{ ...metadata, row_count: -1 }, complete],
  ];
  for (const transcript of transcripts) {
    await server((request, response) => {
      response.writeHead(200, { "content-type": "application/x-ndjson", "x-request-id": "request-test" });
      response.end(transcript.map(JSON.stringify).join("\n"));
    }, async (origin) => {
      const stream = await new uqa.HttpEngine(origin, "token").sqlStream("SELECT 1");
      await assert.rejects(async () => { for await (const frame of stream) void frame; });
      assert.equal(await stream.nextFrame(), null);
    });
  }
});

test("stopping an HTTP stream closes its response", async () => {
  let closed;
  const closure = new Promise((resolve) => { closed = resolve; });
  await server((request, response) => {
    response.on("close", closed);
    response.writeHead(200, { "content-type": "application/x-ndjson", "x-request-id": "request-test" });
    response.write(JSON.stringify(metadata) + "\n");
  }, async (origin) => {
    const stream = await new uqa.HttpEngine(origin, "token").sqlStream("SELECT 1");
    for await (const frame of stream) { assert.equal(frame.type, "metadata"); break; }
    await closure;
  });
});

test("HTTP error envelopes are redacted and redirects are not followed", async () => {
  let requests = 0;
  await server((request, response) => {
    requests += 1;
    json(response, { request_id: "request-test", error: { code: "FAIL", message: "private server detail" } },
      { status: 307, headers: { location: "/must-not-follow" } });
  }, async (origin) => {
    await assert.rejects(new uqa.HttpEngine(origin, "private-token").sql("private sql"), (error) => {
      assert.equal(error.status, 307);
      assert.equal(error.code, "FAIL");
      assert.equal(error.requestId, "request-test");
      for (const secret of ["private-token", "private sql", "private server detail"]) {
        assert.equal(inspect(error).includes(secret), false);
      }
      return true;
    });
    assert.equal(requests, 1);
  });
});

test("HTTP validates origin, malformed responses, lengths, and counters", async () => {
  for (const url of ["http://example.com", "https://user:pass@example.com", "https://example.com/path", "https://example.com/?token=x", "https://example.com/#x"]) {
    assert.throws(() => new uqa.HttpEngine(url, "token"));
  }
  for (const [payload, headers] of [
    ['{"request_id":"wrong"}', {}],
    [result(), { "content-type": "text/plain" }],
    [result(), { "content-length": 70 * 1024 * 1024 }],
    ['{"request_id":"request-test","columns":[],"rows":[],"affected_rows":9007199254740993}', {}],
    ['{"request_id":"request-test","columns":[],"rows":[],"affected_rows":1.0}', {}],
    ['{"request_id":"request-test","columns":[],"rows":[', {}],
    [Buffer.from([0xff]), {}],
  ]) {
    await server((request, response) => {
      response.writeHead(200, { "content-type": "application/json", "x-request-id": "request-test", ...headers });
      response.end(typeof payload === "string" || Buffer.isBuffer(payload) ? payload : JSON.stringify(payload));
    }, async (origin) => {
      await assert.rejects(new uqa.HttpEngine(origin, "token").sql("SELECT 1"));
    });
  }
});

test("CLI project lookup retains literal arguments and excludes UQA_TOKEN", { skip: process.platform === "win32" }, async () => {
  const cli = join(directory, "fake-uqa");
  await server((request, response) => json(response, result()), async (origin) => {
    writeFileSync(cli, [
      "#!/bin/sh",
      'test -z "$UQA_TOKEN" || exit 31',
      'test "$1" = cloud && test "$2" = connection || exit 32',
      'test "$3" = \'a;$(false)\' || exit 33',
      'test "$4" = --format && test "$5" = json || exit 34',
      'test "$6" = --org && test "$7" = organization || exit 35',
      "printf '%s' '" + JSON.stringify({ url: origin, token: "token" }) + "'",
      "",
    ].join("\n"));
    chmodSync(cli, 0o700);
    const previous = process.env.UQA_TOKEN;
    process.env.UQA_TOKEN = "must-not-reach-child";
    try {
      const engine = await uqa.HttpEngine.cloud("a;$(false)", { cliPath: cli, organization: "organization" });
      assert.deepEqual((await engine.sql("SELECT 1")).rows, [{ n: 1 }]);
    } finally {
      if (previous === undefined) delete process.env.UQA_TOKEN;
      else process.env.UQA_TOKEN = previous;
    }
  });
});

test("CLI failures and oversized output do not expose captured secrets", { skip: process.platform === "win32" }, async () => {
  const cli = join(directory, "failed-uqa");
  for (const program of [
    'echo "private token" >&2; exit 1',
    'i=0; while test "$i" -lt 10000; do printf "private token"; i=$((i + 1)); done',
    'printf "private malformed JSON"',
  ]) {
    writeFileSync(cli, "#!/bin/sh\n" + program + "\n");
    chmodSync(cli, 0o700);
    await assert.rejects(uqa.HttpEngine.local("project", { cliPath: cli }), (error) => {
      for (const secret of ["private token", "private malformed JSON"]) {
        assert.equal(inspect(error).includes(secret), false);
      }
      return true;
    });
  }
});

test("packed npm package installs offline and runs HTTP without native artifacts", async () => {
  const npm = [
    resolve(dirname(process.execPath), "../lib/node_modules/npm/bin/npm-cli.js"),
    resolve(dirname(process.execPath), "node_modules/npm/bin/npm-cli.js"),
  ].find(existsSync);
  assert.notEqual(npm, undefined, "the Node.js distribution must include npm");
  const options = { env: { ...process.env, npm_config_cache: join(directory, "npm-cache") }, maxBuffer: 2 * 1024 * 1024 };
  await exec(process.execPath, [npm, "pack", "--json", "--ignore-scripts", "--pack-destination", directory], { ...options, cwd: packagePath });
  const archives = readdirSync(directory).filter((name) => name.endsWith(".tgz"));
  assert.equal(archives.length, 1);
  const archive = join(directory, archives[0]);
  const install = join(directory, "installed");
  mkdirSync(install);
  writeFileSync(join(install, "package.json"), '{"private":true}');
  await exec(process.execPath, [npm, "install", "--offline", "--ignore-scripts", "--omit=optional", "--no-audit", "--no-fund", archive], { ...options, cwd: install });
  const script = join(install, "smoke.mjs");
  writeFileSync(script, [
    'import { HttpEngine, SQLParam } from "@cognica-io/uqa";',
    'import { HttpEngine as Direct } from "@cognica-io/uqa/http";',
    'import assert from "node:assert/strict";',
    'assert.equal(Direct, HttpEngine);',
    'const result = await new HttpEngine(process.argv[2], "token").sql("SELECT $1", [SQLParam.scalar(7)]);',
    'assert.equal(result.rows[0].n, 1);',
  ].join("\n"));
  await server((request, response) => json(response, result()), async (origin) => {
    await exec(process.execPath, [script, origin], options);
  });
});
