//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { protocol } = require("./notification-error.js");
const { MAX_WIRE_BYTES } = require("./notification-request.js");
const bom = [0xef, 0xbb, 0xbf];

class Framer {
  #bytes = new Uint8Array(MAX_WIRE_BYTES);
  #length = 0;
  #lineStart = 0;
  #pendingLF = null;
  #prefix = 0;
  #atStart = true;
  #deferred = false;

  #push(byte) {
    if (this.#length === MAX_WIRE_BYTES) throw protocol("byte_limit");
    this.#bytes[this.#length++] = byte;
  }
  next(input) {
    let consumed = 0;
    for (;;) {
      if (this.#deferred) {
        if (input[consumed] === 10) throw protocol("byte_limit");
        if (consumed === input.length) return { consumed, complete: false };
        this.#deferred = false;
        return { consumed, complete: true };
      }
      if (consumed === input.length) return { consumed, complete: false };
      const byte = input[consumed++];
      if (this.#atStart) {
        if (byte === bom[this.#prefix]) {
          this.#prefix += 1;
          if (this.#prefix === bom.length) { this.#atStart = false; this.#prefix = 0; }
          continue;
        }
        this.#atStart = false;
        for (let index = 0; index < this.#prefix; index += 1) this.#push(bom[index]);
        this.#prefix = 0;
      }
      const pending = this.#pendingLF;
      this.#pendingLF = null;
      if (byte === 10 && pending === "frame") {
        this.#push(byte);
        this.#lineStart = this.#length;
        continue;
      }
      if (byte === 10 && pending === "after") continue;
      this.#push(byte);
      if (byte !== 10 && byte !== 13) continue;
      const empty = this.#lineStart === this.#length - 1;
      this.#lineStart = this.#length;
      if (byte === 13) this.#pendingLF = empty ? "after" : "frame";
      if (empty) {
        if (byte === 13 && this.#length === MAX_WIRE_BYTES) this.#deferred = true;
        else return { consumed, complete: true };
      }
    }
  }
  clear() { this.#length = 0; this.#lineStart = 0; }
  get length() { return this.#length; }
  completeAtEnd() { const result = this.#deferred; this.#deferred = false; return result; }
  consumeTerminalLF(input) {
    if (this.#pendingLF === "after" && input[0] === 10) { this.#pendingLF = null; return 1; }
    return 0;
  }
  endError() {
    try {
      new TextDecoder("utf-8", { fatal: true }).decode(this.#bytes.subarray(0, this.#length), { stream: true });
      return "unexpected_end";
    } catch { return "invalid_utf8"; }
  }
  fields() {
    let text;
    try {
      // Only the initial stream BOM is special; later BOMs are literal input.
      text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(this.#bytes.subarray(0, this.#length));
    } catch { throw protocol("invalid_utf8"); }
    let event;
    let data;
    for (const line of text.split(/[\r\n]/)) {
      if (line === "" || line.startsWith(":")) continue;
      const split = line.indexOf(":");
      const name = split === -1 ? line : line.slice(0, split);
      let value = split === -1 ? "" : line.slice(split + 1);
      if (value.startsWith(" ")) value = value.slice(1);
      if (name === "event") event = value;
      else if (name === "data") data = data === undefined ? value : data + "\n" + value;
      else throw protocol("invalid_fields");
    }
    if (event === undefined && data === undefined) return null;
    if (event === undefined || data === undefined) throw protocol("invalid_fields");
    return { event, data };
  }
}

module.exports = { Framer };
