//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { spawn } = require("node:child_process");
const { HttpEngineError } = require("./http-error.js");

const MAX_OUTPUT_BYTES = 64 * 1024;

function resolveProject(kind, project, options = {}) {
  if (typeof project !== "string" || project.trim() === "") throw new HttpEngineError("UQA project name must not be empty");
  const { organization, cliPath = "uqa" } = options ?? {};
  if (kind === "cloud" && organization != null && (typeof organization !== "string" || organization.trim() === "")) {
    throw new HttpEngineError("UQA organization name must not be empty");
  }
  const args = [kind, "connection", project, "--format", "json"];
  if (kind === "cloud" && organization != null) args.push("--org", organization);
  return new Promise((resolve, reject) => {
    const env = { ...process.env };
    delete env.UQA_TOKEN;
    const chunks = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let settled = false;
    let child;
    try {
      child = spawn(cliPath, args, { env, shell: false, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    } catch {
      reject(new HttpEngineError("UQA CLI is unavailable"));
      return;
    }
    const erase = () => {
      for (const chunk of chunks) chunk.fill(0);
      chunks.length = 0;
    };
    const fail = (message) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      // A descendant can retain inherited pipes after the selected CLI exits.
      // The lookup deadline and output bound must not wait for that process.
      child.stdout.destroy();
      child.stderr.destroy();
      erase();
      reject(new HttpEngineError(message));
    };
    const timer = setTimeout(() => fail("UQA CLI connection lookup timed out"), 30_000);
    timer.unref();
    child.once("error", () => fail("UQA CLI is unavailable"));
    child.stdout.on("data", (chunk) => {
      if (settled) { chunk.fill(0); return; }
      stdoutBytes += chunk.length;
      if (stdoutBytes <= MAX_OUTPUT_BYTES) chunks.push(chunk);
      else {
        chunk.fill(0);
        fail("UQA CLI connection output exceeded the client safety limit");
      }
    });
    child.stderr.on("data", (chunk) => {
      stderrBytes += chunk.length;
      chunk.fill(0);
      if (stderrBytes > MAX_OUTPUT_BYTES) {
        fail("UQA CLI connection output exceeded the client safety limit");
      }
    });
    child.stdout.once("error", () => fail("UQA CLI connection output is invalid"));
    child.stderr.once("error", () => fail("UQA CLI connection output is invalid"));
    child.once("close", (code) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      const bytes = Buffer.concat(chunks);
      try {
        if (code !== 0) throw new HttpEngineError("UQA CLI connection command failed");
        let connection;
        try { connection = JSON.parse(bytes.toString("utf8")); }
        catch { throw new HttpEngineError("UQA CLI connection output is invalid"); }
        if (typeof connection?.url !== "string" || typeof connection?.token !== "string") {
          throw new HttpEngineError("UQA CLI connection output is invalid");
        }
        resolve({ url: connection.url, token: connection.token });
      } catch (error) {
        reject(error);
      } finally {
        bytes.fill(0);
        erase();
      }
    });
  });
}

module.exports = { resolveProject };
