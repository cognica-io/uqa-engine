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
    let failure;
    let child;
    try {
      child = spawn(cliPath, args, { env, shell: false, stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
    } catch {
      reject(new HttpEngineError("UQA CLI is unavailable"));
      return;
    }
    const timer = setTimeout(() => {
      failure = new HttpEngineError("UQA CLI connection lookup timed out");
      child.kill("SIGKILL");
    }, 30_000);
    timer.unref();
    child.once("error", () => {
      clearTimeout(timer);
      failure = new HttpEngineError("UQA CLI is unavailable");
      reject(failure);
    });
    child.stdout.on("data", (chunk) => {
      stdoutBytes += chunk.length;
      if (stdoutBytes <= MAX_OUTPUT_BYTES) chunks.push(chunk);
      else {
        failure = new HttpEngineError("UQA CLI connection output exceeded the client safety limit");
        child.kill("SIGKILL");
      }
    });
    child.stderr.on("data", (chunk) => {
      stderrBytes += chunk.length;
      if (stderrBytes > MAX_OUTPUT_BYTES) {
        failure = new HttpEngineError("UQA CLI connection output exceeded the client safety limit");
        child.kill("SIGKILL");
      }
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      const bytes = Buffer.concat(chunks);
      try {
        if (failure !== undefined) throw failure;
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
        for (const chunk of chunks) chunk.fill(0);
      }
    });
  });
}

module.exports = { resolveProject };
