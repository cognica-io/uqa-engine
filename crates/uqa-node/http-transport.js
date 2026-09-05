//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const http = require("node:http");
const https = require("node:https");
const { TextDecoder } = require("node:util");
const { HttpEngineError, invalidResponse } = require("./http-error.js");
const { parseJSON, stringifyJSON } = require("./http-json.js");

const MAX_JSON_BYTES = 65 * 1024 * 1024;
const MAX_ERROR_BYTES = 64 * 1024;
const MAX_FRAME_BYTES = 64 * 1024 * 1024;

function baseURL(source) {
  let url;
  try { url = new URL(source); } catch { throw new HttpEngineError("UQA data-plane URL is invalid"); }
  if (typeof source !== "string" || url.username || url.password || url.pathname !== "/" ||
      url.search || url.hash || !["http:", "https:"].includes(url.protocol)) {
    throw new HttpEngineError("UQA data-plane URL is invalid");
  }
  const octets = url.hostname.split(".");
  const loopback = ["localhost", "[::1]"].includes(url.hostname) ||
    (octets.length === 4 && octets[0] === "127" && octets.every((part) => /^\d+$/.test(part) && Number(part) <= 255));
  if (url.protocol === "http:" && !loopback) {
    throw new HttpEngineError("plain HTTP UQA URLs must resolve to loopback");
  }
  return url;
}

function request(origin, token, path, body, accept = "application/json") {
  const data = stringifyJSON(body);
  return new Promise((resolve, reject) => {
    let outgoing;
    let timer;
    const fail = () => reject(new HttpEngineError("UQA HTTP transport failed"));
    try {
      outgoing = (origin.protocol === "https:" ? https : http).request(new URL(path, origin), {
        method: "POST",
        agent: false,
        headers: {
          authorization: "Bearer " + token,
          "content-type": "application/json",
          accept,
          "content-length": Buffer.byteLength(data),
        },
      }, (response) => {
        clearTimeout(timer);
        resolve(response);
      });
      timer = setTimeout(() => outgoing.destroy(), 10_000);
      timer.unref();
      outgoing.once("socket", (socket) => {
        const connected = () => clearTimeout(timer);
        if (!socket.connecting) connected();
        else socket.once(origin.protocol === "https:" ? "secureConnect" : "connect", connected);
      });
      outgoing.once("error", () => { clearTimeout(timer); fail(); });
      outgoing.end(data);
    } catch {
      clearTimeout(timer);
      outgoing?.destroy();
      fail();
    }
  });
}

function requestId(response) {
  const id = response.headers["x-request-id"];
  if (typeof id !== "string" || id.length === 0) {
    response.destroy();
    throw new HttpEngineError("UQA response is missing its request ID");
  }
  return id;
}

function contentType(response, expected) {
  const type = response.headers["content-type"]?.split(";", 1)[0].trim().toLowerCase();
  if (type !== expected) {
    response.destroy();
    throw new HttpEngineError("UQA response content type is invalid");
  }
}

function decodeJSON(bytes) {
  try { return parseJSON(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); }
  catch { throw invalidResponse(); }
}

async function boundedBody(response, limit) {
  const chunks = [];
  let length = 0;
  try {
    if (Number(response.headers["content-length"]) > limit) throw new HttpEngineError("UQA response exceeded the client safety limit");
    for await (const chunk of response) {
      length += chunk.length;
      if (length > limit) throw new HttpEngineError("UQA response exceeded the client safety limit");
      chunks.push(chunk);
    }
    return Buffer.concat(chunks, length);
  } catch (error) {
    response.destroy();
    throw error instanceof HttpEngineError ? error : new HttpEngineError("UQA HTTP transport failed");
  }
}

function succeeded(response) {
  return response.statusCode >= 200 && response.statusCode < 300;
}

async function jsonResponse(response) {
  const id = requestId(response);
  contentType(response, "application/json");
  const body = decodeJSON(await boundedBody(response, succeeded(response) ? MAX_JSON_BYTES : MAX_ERROR_BYTES));
  if (body === null || typeof body !== "object" || Array.isArray(body)) throw invalidResponse();
  if ((succeeded(response) || body.request_id !== undefined) && body.request_id !== id) {
    throw new HttpEngineError("UQA response request IDs do not match");
  }
  if (!succeeded(response)) {
    const code = typeof body.error?.code === "string" ? body.error.code : "HTTP_ERROR";
    throw new HttpEngineError("UQA returned " + response.statusCode + " with code " + code, {
      code, status: response.statusCode, requestId: id,
    });
  }
  return { body, requestId: id };
}

module.exports = { baseURL, request, requestId, contentType, decodeJSON, jsonResponse, succeeded, MAX_FRAME_BYTES };
