//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { NotificationError, NotificationEvent } = require("./notification-error.js");
const { positive } = require("./notification-request.js");
const { byteLength, encodeInto, decode } = require("./notification-bytes.js");

// Queue ownership is encoded bytes plus fixed reference slots. JS object and
// allocator overhead are additionally bounded by the event count, not guessed
// as a byte-level whole-process memory allowance.
class NotificationQueue {
  #items;
  #head = 0;
  #count = 0;
  #bytes;
  #maximum;
  constructor(maximumCount, maximumBytes) {
    positive(maximumCount, 0xffffffff);
    positive(maximumBytes);
    const slots = maximumCount * 8;
    if (slots > maximumBytes) throw new NotificationError("NOTIFICATION_CAPACITY");
    this.#items = new Array(maximumCount);
    this.#bytes = slots;
    this.#maximum = maximumBytes;
  }
  get length() { return this.#count; }
  get chargedBytes() { return this.#bytes; }
  push(event) {
    const notification = event.kind === "notification";
    const tag = notification ? 1 : event.kind === "resync_required" ? 2 : 3;
    const requestId = event.requestId ?? "";
    const channelBytes = notification ? byteLength(event.channel) : 0;
    const text = notification ? event.payload : event.cause ?? "";
    const textBytes = byteLength(text);
    const length = 18 + requestId.length + (notification ? 15 + channelBytes : 2) + textBytes;
    if (this.#count === this.#items.length || length > this.#maximum - this.#bytes) {
      throw new NotificationError("NOTIFICATION_BACKPRESSURE");
    }
    // Each record owns exactly its admitted byte buffer, without a shared slab.
    const bytes = new Uint8Array(length);
    const view = new DataView(bytes.buffer);
    bytes[0] = tag;
    const epoch = event.epoch.replaceAll("-", "");
    for (let index = 0; index < 16; index += 1) bytes[index + 1] = Number.parseInt(epoch.slice(index * 2, index * 2 + 2), 16);
    bytes[17] = requestId.length;
    let offset = 18;
    offset += encodeInto(requestId, bytes.subarray(offset, offset + requestId.length));
    if (notification) {
      view.setBigUint64(offset, event.sequence); offset += 8;
      view.setInt32(offset, event.processId); offset += 4;
      bytes[offset++] = channelBytes;
      view.setUint16(offset, textBytes); offset += 2;
      offset += encodeInto(event.channel, bytes.subarray(offset, offset + channelBytes));
    } else {
      view.setUint16(offset, textBytes); offset += 2;
    }
    encodeInto(text, bytes.subarray(offset, offset + textBytes));
    this.#items[(this.#head + this.#count) % this.#items.length] = bytes;
    this.#bytes += bytes.length;
    this.#count += 1;
  }
  take() {
    if (this.#count === 0) return null;
    const bytes = this.#items[this.#head];
    this.#items[this.#head] = undefined;
    this.#head = (this.#head + 1) % this.#items.length;
    this.#count -= 1;
    this.#bytes -= bytes.length;
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let hex = "";
    for (let index = 1; index < 17; index += 1) hex += bytes[index].toString(16).padStart(2, "0");
    const value = {
      kind: bytes[0] === 1 ? "notification" : bytes[0] === 2 ? "resync_required" : "reconnected",
      epoch: hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20),
      requestId: bytes[17] === 0 ? null : decode(bytes.subarray(18, 18 + bytes[17])),
    };
    let offset = 18 + bytes[17];
    if (bytes[0] === 1) {
      value.sequence = view.getBigUint64(offset); offset += 8;
      value.processId = view.getInt32(offset); offset += 4;
      const channelBytes = bytes[offset++];
      const payloadBytes = view.getUint16(offset); offset += 2;
      value.channel = decode(bytes.subarray(offset, offset + channelBytes)); offset += channelBytes;
      value.payload = decode(bytes.subarray(offset, offset + payloadBytes));
    } else {
      const length = view.getUint16(offset); offset += 2;
      if (value.kind === "resync_required") value.cause = decode(bytes.subarray(offset, offset + length));
    }
    return new NotificationEvent(value);
  }
  clear() {
    this.#items = [];
    this.#count = 0;
    this.#head = 0;
    this.#bytes = 0;
  }
  [Symbol.for("nodejs.util.inspect.custom")]() { return "NotificationQueue(...)"; }
}

module.exports = { NotificationQueue };
