//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { Framer } = require("./notification-framing.js");
const { parseJSON } = require("./http-json.js");
const { MAX_WIRE_BYTES, MAX_TIMER_MS, positive } = require("./notification-request.js");
const { NotificationError, NotificationEvent, protocol, knownKind } = require("./notification-error.js");
const { byteLength } = require("./notification-bytes.js");
const MAX_SEQUENCE = (1n << 64n) - 1n;
const retryableKinds = new Set(["CAPACITY", "SOURCE_UNAVAILABLE", "TRANSPORT", "TIMEOUT", "SERVER_DRAINING"]
  .map((kind) => "NOTIFICATION_" + kind));

function fullMatch(pattern, value) {
  return typeof value === "string" && pattern.exec(value)?.[0] === value;
}
function requestId(value) {
  if (!fullMatch(/^[A-Za-z0-9_-]{1,128}$/, value)) throw protocol("identity");
  return value;
}
function decimal(value) {
  if (!fullMatch(/^[1-9][0-9]{0,19}$/, value)) throw protocol("decimal");
  const number = BigInt(value);
  if (number > MAX_SEQUENCE) throw protocol("decimal");
  return number;
}
function identity(raw, expected, epoch) {
  if (raw.request_id !== expected || !fullMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/, raw.stream_id) ||
      (epoch !== undefined && epoch !== raw.stream_id)) throw protocol("identity");
  return { requestId: expected, epoch: raw.stream_id };
}
function object(source, fields) {
  let value;
  try { value = parseJSON(source, { rejectDuplicates: true, scalarUnicode: true, maxContainerDepth: 2 }); }
  catch { throw protocol("invalid_json"); }
  if (value === null || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== fields.length || Object.keys(value).some((field) => !fields.includes(field))) {
    throw protocol("invalid_fields");
  }
  return value;
}
function remoteError(raw, id) {
  if (!fullMatch(/^[A-Z0-9_]{1,64}$/, raw.code) || typeof raw.retryable !== "boolean") throw protocol("invalid_fields");
  const code = knownKind(raw.code) ? raw.code : "NOTIFICATION_PROTOCOL";
  return new NotificationError(code, {
    requestId: id, diagnostic: raw.code,
    retryable: raw.retryable && retryableKinds.has(code),
  });
}

class NotificationDecoder {
  #framer = new Framer();
  #request;
  #requestId;
  #maxIdle;
  #ready = null;
  #sequence = 0n;
  #terminal = false;
  #ended = false;
  #failure = null;
  constructor(request, expectedRequestId, maxIdleTimeoutMs) {
    this.#request = request;
    this.#requestId = requestId(expectedRequestId);
    this.#maxIdle = BigInt(positive(maxIdleTimeoutMs, MAX_TIMER_MS));
  }
  get ready() { return this.#ready; }
  get isTerminal() { return this.#terminal || this.#failure !== null; }
  #fail(error) { this.#failure = error; this.#framer.clear(); throw error; }
  decode(input) {
    if (this.#failure !== null) throw this.#failure;
    try {
      if (this.#ended) throw protocol("unexpected_end");
      if (this.#terminal) {
        const consumed = this.#framer.consumeTerminalLF(input);
        if (consumed !== input.length) throw protocol("event_order");
        return { consumed, event: null };
      }
      const { consumed, complete } = this.#framer.next(input);
      return { consumed, event: complete ? this.#admit() : null };
    } catch (error) { return this.#fail(error); }
  }
  finish() {
    if (this.#failure !== null) throw this.#failure;
    this.#ended = true;
    try {
      if (this.#framer.completeAtEnd()) return this.#admit();
      if (this.#terminal && this.#framer.length === 0) return null;
      const reason = this.#framer.endError();
      if (reason === "unexpected_end") {
        throw new NotificationError("NOTIFICATION_TRANSPORT", { diagnostic: reason, retryable: true });
      }
      throw protocol(reason);
    } catch (error) { return this.#fail(error); }
  }
  #admit() {
    const fields = this.#framer.fields();
    const event = fields === null ? Object.freeze({ kind: "heartbeat" }) : this.#envelope(fields);
    if (event.kind === "ready") this.#ready = event;
    if (event.kind === "notification") this.#sequence = event.sequence;
    if (event.kind === "error" || event.kind === "closed") this.#terminal = true;
    this.#framer.clear();
    return event;
  }
  #envelope({ event, data }) {
    if (event === "ready") {
      if (this.#ready !== null) throw protocol("event_order");
      const raw = object(data, ["protocol_version", "request_id", "stream_id", "accepted_channel_count",
        "delivery", "resume_supported", "max_event_bytes", "heartbeat_interval_ms", "idle_timeout_ms", "timing_margin_ms"]);
      if (raw.protocol_version !== 1 || raw.accepted_channel_count !== this.#request.channels.length ||
          raw.delivery !== "live" || raw.resume_supported !== false || raw.max_event_bytes !== MAX_WIRE_BYTES) {
        throw protocol("invalid_fields");
      }
      const id = identity(raw, this.#requestId);
      const heartbeat = decimal(raw.heartbeat_interval_ms);
      const idle = decimal(raw.idle_timeout_ms);
      const margin = decimal(raw.timing_margin_ms);
      if ([heartbeat, idle, margin].some((value) => value > BigInt(MAX_TIMER_MS)) ||
          idle > this.#maxIdle) throw protocol("timer_range");
      if (idle <= 3n * heartbeat + margin) throw protocol("timing");
      return Object.freeze({ kind: "ready", ...id, heartbeatIntervalMs: Number(heartbeat),
        idleTimeoutMs: Number(idle), timingMarginMs: Number(margin) });
    }
    if (this.#ready === null) throw protocol("event_order");
    if (event === "notification") {
      const raw = object(data, ["request_id", "stream_id", "sequence", "process_id", "channel", "payload"]);
      const id = identity(raw, this.#requestId, this.#ready.epoch);
      const sequence = decimal(raw.sequence);
      if (this.#sequence + 1n !== sequence) throw protocol("sequence");
      if (!Number.isInteger(raw.process_id) || Object.is(raw.process_id, -0) ||
          raw.process_id < -2147483648 || raw.process_id > 2147483647 ||
          !this.#request.channels.includes(raw.channel)) throw protocol("invalid_fields");
      if (typeof raw.payload !== "string" || byteLength(raw.payload) > 7999) throw protocol("payload");
      return new NotificationEvent({ kind: "notification", ...id, sequence,
        processId: raw.process_id, channel: raw.channel, payload: raw.payload });
    }
    if (event === "error") {
      const raw = object(data, ["request_id", "stream_id", "code", "retryable"]);
      const id = identity(raw, this.#requestId, this.#ready.epoch);
      return Object.freeze({ kind: "error", ...id, error: remoteError(raw, this.#requestId) });
    }
    if (event === "closed") {
      const raw = object(data, ["request_id", "stream_id", "reason"]);
      const id = identity(raw, this.#requestId, this.#ready.epoch);
      if (raw.reason !== "server_draining") throw protocol("invalid_fields");
      return Object.freeze({ kind: "closed", ...id });
    }
    throw protocol("invalid_fields");
  }
  [Symbol.for("nodejs.util.inspect.custom")]() { return "NotificationDecoder(...)"; }
}

module.exports = { NotificationDecoder, requestId, decimal, remoteError };
