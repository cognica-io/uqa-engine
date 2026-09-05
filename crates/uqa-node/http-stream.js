//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { HttpEngineError, invalidResponse } = require("./http-error.js");
const { decodeRow, count, columns } = require("./http-values.js");
const { decodeJSON, MAX_FRAME_BYTES } = require("./http-transport.js");

async function* lines(response) {
  let chunks = [];
  let length = 0;
  try {
    for await (const chunk of response) {
      let start = 0;
      for (;;) {
        const end = chunk.indexOf(10, start);
        const part = chunk.subarray(start, end === -1 ? chunk.length : end);
        length += part.length;
        if (length > MAX_FRAME_BYTES) throw new HttpEngineError("UQA NDJSON stream frame exceeded the client safety limit");
        if (part.length !== 0) chunks.push(part);
        if (end === -1) break;
        yield Buffer.concat(chunks, length);
        chunks = [];
        length = 0;
        start = end + 1;
      }
    }
    if (length !== 0) yield Buffer.concat(chunks, length);
  } catch (error) {
    throw error instanceof HttpEngineError ? error : new HttpEngineError("UQA HTTP transport failed");
  } finally {
    response.destroy();
  }
}

class HttpSQLStream {
  #requestId;
  #response;
  #frames;
  #pending = Promise.resolve();
  #closed = false;

  constructor(response, requestId) {
    this.#requestId = requestId;
    this.#response = response;
    this.#frames = this.#readFrames();
  }

  get requestId() { return this.#requestId; }

  nextFrame() {
    const next = this.#pending.then(async () => {
      if (this.#closed) return null;
      try {
        const result = await this.#frames.next();
        if (result.done) this.#closed = true;
        return result.done ? null : result.value;
      } catch (error) {
        this.#closed = true;
        this.#response.destroy();
        throw error;
      }
    });
    this.#pending = next.catch(() => {});
    return next;
  }

  async *#readFrames() {
    let phase = "metadata";
    for await (let line of lines(this.#response)) {
      if (line[line.length - 1] === 13) line = line.subarray(0, -1);
      if (line.length === 0) continue;
      if (phase === "terminal") throw new HttpEngineError("UQA NDJSON stream frame order is invalid");
      const frame = decodeJSON(line);
      if (frame === null || typeof frame !== "object" || Array.isArray(frame)) throw invalidResponse();
      if (frame.request_id !== undefined && frame.request_id !== this.#requestId) {
        throw new HttpEngineError("UQA NDJSON stream request ID does not match its HTTP response");
      }
      if (phase === "metadata" && frame.type === "metadata") {
        if (frame.request_id !== this.#requestId || typeof frame.spilled_to_disk !== "boolean") throw invalidResponse();
        phase = "rows";
        yield { type: "metadata", columns: columns(frame.columns), rowCount: count(frame.row_count),
          spilledToDisk: frame.spilled_to_disk, requestId: this.#requestId };
      } else if (phase === "rows" && frame.type === "row") {
        yield { type: "row", row: decodeRow(frame.row) };
      } else if (frame.type === "error") {
        if (frame.request_id !== this.#requestId || typeof frame.code !== "string" || typeof frame.message !== "string") throw invalidResponse();
        phase = "terminal";
        yield { type: "error", code: frame.code, message: frame.message, requestId: this.#requestId };
      } else if (phase === "rows" && frame.type === "complete") {
        if (frame.request_id !== this.#requestId) throw invalidResponse();
        phase = "terminal";
        yield { type: "complete", rowCount: count(frame.row_count), requestId: this.#requestId };
      } else throw new HttpEngineError("UQA NDJSON stream frame order is invalid");
    }
    if (phase !== "terminal") throw new HttpEngineError("UQA NDJSON stream ended before a terminal frame");
  }

  [Symbol.asyncIterator]() {
    return {
      next: async () => {
        const value = await this.nextFrame();
        return value === null ? { done: true, value: undefined } : { done: false, value };
      },
      return: async () => {
        this.#closed = true;
        this.#response.destroy();
        await this.#pending;
        await this.#frames.return();
        return { done: true, value: undefined };
      },
    };
  }
}

module.exports = { HttpSQLStream };
