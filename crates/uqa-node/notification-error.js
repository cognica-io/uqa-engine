//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const inspect = Symbol.for("nodejs.util.inspect.custom");
const kinds = new Set([
  "INVALID_REQUEST", "AUTHENTICATION", "AUTHORITY_REVOKED", "UNSUPPORTED",
  "CAPACITY", "BACKPRESSURE", "PROTOCOL", "SOURCE_UNAVAILABLE", "TRANSPORT",
  "TIMEOUT", "SERVER_DRAINING", "CANCELLED", "SEQUENCE_EXHAUSTED",
].map((kind) => "NOTIFICATION_" + kind));

class NotificationError extends Error {
  #details;
  constructor(code, details = {}) {
    if (!kinds.has(code)) throw new TypeError("invalid notification failure kind");
    super(code);
    this.name = "NotificationError";
    Object.defineProperty(this, "code", { value: code, enumerable: true });
    this.#details = Object.freeze({ ...details });
  }
  get httpStatus() { return this.#details.httpStatus ?? null; }
  get requestId() { return this.#details.requestId ?? null; }
  get originalFailure() { return this.#details.originalFailure ?? null; }
  get lastAttemptFailure() { return this.#details.lastAttemptFailure ?? null; }
  get reconnectAttempts() { return this.#details.reconnectAttempts ?? null; }
  get diagnostic() { return this.#details.diagnostic ?? null; }
  get timeoutStage() { return this.#details.timeoutStage ?? null; }
  get retryable() { return this.#details.retryable === true; }
  get retryAfterMs() { return this.#details.retryAfterMs ?? null; }
  [inspect]() { return "NotificationError(" + this.code + ")"; }
}

class NotificationEvent {
  #value;
  constructor(value) { this.#value = Object.freeze({ ...value }); Object.freeze(this); }
  get kind() { return this.#value.kind; }
  get epoch() { return this.#value.epoch; }
  get requestId() { return this.#value.requestId ?? null; }
  get sequence() { return this.#value.sequence ?? null; }
  get processId() { return this.#value.processId ?? null; }
  get channel() { return this.#value.channel ?? null; }
  get payload() { return this.#value.payload ?? null; }
  get cause() { return this.#value.cause ?? null; }
  [inspect]() { return "NotificationEvent(" + this.kind + ")"; }
}

function protocol(reason) {
  return new NotificationError("NOTIFICATION_PROTOCOL", { diagnostic: reason });
}

function knownKind(code) { return kinds.has(code); }
function cancelled() { return new NotificationError("NOTIFICATION_CANCELLED"); }
function timeout(stage) { return new NotificationError("NOTIFICATION_TIMEOUT", { timeoutStage: stage, retryable: true }); }

module.exports = { NotificationError, NotificationEvent, protocol, knownKind, cancelled, timeout };
