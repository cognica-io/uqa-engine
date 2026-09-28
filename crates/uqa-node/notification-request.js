//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { validUnicode } = require("./http-json.js");
const { NotificationError } = require("./notification-error.js");
const { byteLength, encode, decode, compare } = require("./notification-bytes.js");
const MAX_WIRE_BYTES = 65536;
const MAX_TIMER_MS = 2147483647;

function invalid() { return new NotificationError("NOTIFICATION_INVALID_REQUEST"); }
function positive(value, maximum = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value <= 0 || value > maximum) throw invalid();
  return value;
}

function subscriptionRequest(channels, maximum) {
  positive(maximum);
  if (!Array.isArray(channels) || channels.length === 0 ||
      channels.length > maximum || channels.length > MAX_WIRE_BYTES / 4) throw invalid();
  const values = [];
  const seen = new Set();
  let bytes = byteLength('{"protocol_version":1,"channels":[]}');
  for (let index = 0, count = channels.length; index < count; index += 1) {
    const channel = channels[index];
    if (!validUnicode(channel) || channel.length === 0 || channel.length > 63 ||
        channel.includes("\0") || seen.has(channel)) throw invalid();
    const utf8 = encode(channel);
    if (utf8.length > 63) throw invalid();
    // A caller's short sliced string may retain an arbitrarily large parent.
    const owned = decode(utf8);
    const encoded = JSON.stringify(owned);
    bytes += byteLength(encoded) + (index === 0 ? 0 : 1);
    if (bytes > MAX_WIRE_BYTES) throw invalid();
    seen.add(channel);
    values.push({ channel: owned, utf8, encoded });
  }
  values.sort((left, right) => compare(left.utf8, right.utf8));
  return Object.freeze({
    channels: Object.freeze(values.map((value) => value.channel)),
    body: '{"protocol_version":1,"channels":[' + values.map((value) => value.encoded).join(",") + "]}",
  });
}

function httpOptions(options) {
  if (options === null || typeof options !== "object") throw invalid();
  const output = {};
  for (const field of ["maxChannels", "maxQueuedEvents", "maxQueuedBytes", "maxTransportChunkBytes"]) {
    output[field] = positive(options[field]);
  }
  for (const field of ["connectTimeoutMs", "readyTimeoutMs", "maxIdleTimeoutMs"]) {
    output[field] = positive(options[field], MAX_TIMER_MS);
  }
  if (output.connectTimeoutMs > output.readyTimeoutMs || output.maxTransportChunkBytes < MAX_WIRE_BYTES) throw invalid();
  if (options.retry != null) {
    const policy = options.retry;
    output.retry = { maxAttempts: positive(policy.maxAttempts, 0xffffffff) };
    for (const field of ["episodeTimeoutMs", "initialBackoffMs", "maxBackoffMs", "maxRetryAfterMs"]) {
      output.retry[field] = positive(policy[field], MAX_TIMER_MS);
    }
    if (output.retry.initialBackoffMs > output.retry.maxBackoffMs) throw invalid();
    Object.freeze(output.retry);
  } else output.retry = null;
  return Object.freeze(output);
}

module.exports = { subscriptionRequest, httpOptions, positive, invalid, MAX_WIRE_BYTES, MAX_TIMER_MS };
