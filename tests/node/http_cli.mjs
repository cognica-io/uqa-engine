//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { test } from "node:test";
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { join } from "node:path";

function resolver(packagePath) {
  const child = new EventEmitter();
  child.stdout = new PassThrough();
  child.stderr = new PassThrough();
  const kills = [];
  child.kill = (signal) => { kills.push(signal); return false; };
  let deadline;
  let cleared = false;
  let spawnOptions;
  const module = { exports: {} };
  const require = createRequire(join(packagePath, "http-cli.js"));
  runInNewContext(readFileSync(join(packagePath, "http-cli.js"), "utf8"), {
    module, Buffer, process: { env: { UQA_TOKEN: "parent credential", PATH: "fixture path" } },
    require: (name) => name === "node:child_process" ? {
      spawn: (path, args, options) => { spawnOptions = { path, args, ...options }; return child; },
    } : require(name),
    setTimeout: (callback, milliseconds) => {
      assert.equal(milliseconds, 30_000);
      deadline = callback;
      return { unref() {} };
    },
    clearTimeout: () => { cleared = true; },
  });
  const pending = module.exports.resolveProject("cloud", "a;$(false)", { organization: "organization" });
  return { child, kills, pending, deadline: () => deadline(), cleared: () => cleared,
    spawnOptions, HttpEngineError: require("./http-error.js").HttpEngineError };
}

export function registerCLILifecycleTests(packagePath) {
  test("CLI deadline rejects even when an exited child leaves its output pipe open", async () => {
    const fixture = resolver(packagePath);
    const credential = Buffer.from('{"token":"private credential"');
    fixture.child.stdout.emit("data", credential);
    fixture.child.emit("exit", 0, null);
    let error;
    fixture.pending.catch((reason) => { error = reason; });
    fixture.deadline();
    await Promise.resolve();
    assert.ok(error instanceof fixture.HttpEngineError);
    assert.equal(error.message, "UQA CLI connection lookup timed out");
    assert.deepEqual(fixture.kills, ["SIGKILL"]);
    assert.equal(fixture.child.stdout.destroyed, true);
    assert.equal(fixture.child.stderr.destroyed, true);
    assert.equal(fixture.cleared(), true);
    assert.ok(credential.every((byte) => byte === 0));
    fixture.child.emit("close", 0);
    assert.equal(error.message, "UQA CLI connection lookup timed out");
  });

  for (const stream of ["stdout", "stderr"]) {
    test(`CLI ${stream} overflow fails before pipe closure and erases retained credentials`, async () => {
      const fixture = resolver(packagePath);
      const retained = Buffer.from("private token");
      fixture.child.stdout.emit("data", retained);
      const overflow = Buffer.alloc(64 * 1024 + 1, 97);
      let error;
      fixture.pending.catch((reason) => { error = reason; });
      fixture.child[stream].emit("data", overflow);
      await Promise.resolve();
      assert.ok(error instanceof fixture.HttpEngineError);
      assert.match(error.message, /output exceeded the client safety limit/);
      assert.deepEqual(fixture.kills, ["SIGKILL"]);
      assert.equal(fixture.child.stdout.destroyed, true);
      assert.equal(fixture.child.stderr.destroyed, true);
      assert.ok(retained.every((byte) => byte === 0));
      assert.ok(overflow.every((byte) => byte === 0));
      fixture.deadline();
      fixture.child.emit("close", 1);
      assert.deepEqual(fixture.kills, ["SIGKILL"]);
    });

    test(`CLI ${stream} read errors reject with redacted diagnostics and release the child`, async () => {
      const fixture = resolver(packagePath);
      fixture.child[stream].emit("error", new Error("private token"));
      await assert.rejects(fixture.pending, (error) => {
        assert.ok(error instanceof fixture.HttpEngineError);
        assert.equal(error.message, "UQA CLI connection output is invalid");
        return true;
      });
      assert.equal(fixture.child.stdout.destroyed, true);
      assert.equal(fixture.child.stderr.destroyed, true);
      assert.deepEqual(fixture.kills, ["SIGKILL"]);
    });
  }

  test("CLI spawn failure erases collected credentials before a delayed close", async () => {
    const fixture = resolver(packagePath);
    const bytes = Buffer.from("private token");
    fixture.child.stdout.emit("data", bytes);
    fixture.child.emit("error", new Error("private CLI path"));
    await assert.rejects(fixture.pending, (error) => {
      assert.ok(error instanceof fixture.HttpEngineError);
      assert.equal(error.message, "UQA CLI is unavailable");
      return true;
    });
    assert.ok(bytes.every((byte) => byte === 0));
    assert.equal(fixture.cleared(), true);
    fixture.child.emit("close", -1);
  });

  test("CLI successful completion preserves literal arguments, excludes credentials and clears its timer", async () => {
    const fixture = resolver(packagePath);
    const bytes = Buffer.from('{"url":"http://127.0.0.1:1234","token":"private token"}');
    fixture.child.stdout.emit("data", bytes);
    fixture.child.emit("close", 0);
    const result = await fixture.pending;
    assert.equal(result.url, "http://127.0.0.1:1234");
    assert.equal(result.token, "private token");
    assert.deepEqual(Array.from(fixture.spawnOptions.args), ["cloud", "connection", "a;$(false)", "--format", "json", "--org", "organization"]);
    assert.equal(fixture.spawnOptions.env.UQA_TOKEN, undefined);
    assert.equal(fixture.spawnOptions.env.PATH, "fixture path");
    assert.equal(fixture.spawnOptions.shell, false);
    assert.deepEqual(Array.from(fixture.spawnOptions.stdio), ["ignore", "pipe", "pipe"]);
    assert.equal(fixture.cleared(), true);
    assert.ok(bytes.every((byte) => byte === 0));
    fixture.deadline();
    assert.deepEqual(fixture.kills, []);
  });
}
