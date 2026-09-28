//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { parseJSON } = require("./http-json.js");
const { NotificationError, protocol } = require("./notification-error.js");
const { requestId } = require("./notification-protocol.js");
const { MAX_WIRE_BYTES, MAX_TIMER_MS } = require("./notification-request.js");

function values(response, name) {
  const result = [];
  for (let index = 0; index < response.rawHeaders.length; index += 2) {
    if (response.rawHeaders[index].toLowerCase() !== name) continue;
    const value = response.rawHeaders[index + 1];
    if (/[^\t\x20-\x7e]/.test(value)) throw protocol("invalid_headers");
    result.push(value);
  }
  return result;
}
function single(response, name) {
  const found = values(response, name);
  if (found.length > 1) throw protocol("invalid_headers");
  return found[0] ?? null;
}
function identityEncoding(response) {
  const encoding = single(response, "content-encoding");
  if (encoding !== null && encoding.trim().toLowerCase() !== "identity") throw protocol("invalid_headers");
}
function streamHeaders(response) {
  identityEncoding(response);
  const parts = single(response, "content-type")?.split(";");
  if (parts?.length !== 2 || parts[0].trim().toLowerCase() !== "text/event-stream" ||
      !/^charset\s*=\s*(?:utf-8|"utf-8")$/i.test(parts[1].trim())) throw protocol("invalid_headers");
  const cache = values(response, "cache-control").flatMap((value) => value.toLowerCase().split(",").map((part) => part.trim()));
  if (!cache.includes("no-store") || !cache.includes("no-transform")) throw protocol("invalid_headers");
  return requestId(single(response, "x-request-id"));
}

// Validate the three HTTP-date grammars, including the calendar and weekday.
// Date.parse alone also accepts unrelated and normalized invalid dates.
function httpDate(raw) {
  const months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const text = raw.trim();
  let match = /^(Sun|Mon|Tue|Wed|Thu|Fri|Sat), (\d{2}) (\w{3}) (\d{4}) (\d{2}):(\d{2}):(\d{2}) GMT$/.exec(text);
  let day, month, year, hour, minute, second, weekday;
  if (match) [, weekday, day, month, year, hour, minute, second] = match;
  else {
    match = /^(Sunday|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday), (\d{2})-(\w{3})-(\d{2}) (\d{2}):(\d{2}):(\d{2}) GMT$/.exec(text);
    if (match) {
      [, weekday, day, month, year, hour, minute, second] = match;
      weekday = weekday.slice(0, 3);
      year = Number(year) + (Number(year) < 70 ? 2000 : 1900);
    } else {
      match = /^(Sun|Mon|Tue|Wed|Thu|Fri|Sat) (\w{3}) ([ \d]\d) (\d{2}):(\d{2}):(\d{2}) (\d{4})$/.exec(text);
      if (!match) throw protocol("retry_after");
      [, weekday, month, day, hour, minute, second, year] = match;
    }
  }
  const numbers = [Number(year), months.indexOf(month), Number(day), Number(hour), Number(minute), Number(second)];
  if (numbers[0] < 1970 || numbers[1] < 0) throw protocol("retry_after");
  const result = new Date(Date.UTC(...numbers));
  const actual = [result.getUTCFullYear(), result.getUTCMonth(), result.getUTCDate(),
    result.getUTCHours(), result.getUTCMinutes(), result.getUTCSeconds()];
  if (actual.some((value, index) => value !== numbers[index]) || days[result.getUTCDay()] !== weekday) throw protocol("retry_after");
  return result.getTime();
}

function retryAfter(response, options, now = Date.now()) {
  const raw = single(response, "retry-after");
  if (raw === null) return null;
  if (raw.length === 0 || raw.length > 128) throw protocol("retry_after");
  const delay = /^[0-9]+$/.test(raw) ? BigInt(raw) * 1000n : BigInt(Math.max(0, httpDate(raw) - now));
  if (delay > BigInt(MAX_TIMER_MS) || (options.retry !== null && delay > BigInt(options.retry.maxRetryAfterMs))) throw protocol("timer_range");
  return Number(delay);
}

function exactObject(value, fields) {
  if (value === null || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== fields.length || fields.some((field) => !Object.prototype.hasOwnProperty.call(value, field))) throw protocol("invalid_fields");
}

async function responseError(response, options, check) {
  const status = response.statusCode;
  if ([404, 405, 501].includes(status)) return new NotificationError("NOTIFICATION_UNSUPPORTED");
  if (status >= 300 && status < 400) throw protocol("redirect");
  identityEncoding(response);
  const id = requestId(single(response, "x-request-id"));
  if (single(response, "content-type")?.split(";", 1)[0].trim().toLowerCase() !== "application/json") throw protocol("invalid_headers");
  const retryAfterMs = retryAfter(response, options);
  const length = single(response, "content-length");
  if (length !== null && (!/^[0-9]+$/.test(length) || BigInt(length) > BigInt(MAX_WIRE_BYTES))) throw protocol("byte_limit");
  const bytes = new Uint8Array(MAX_WIRE_BYTES);
  let used = 0;
  for await (const chunk of response) {
    check();
    if (chunk.length > options.maxTransportChunkBytes) throw new NotificationError("NOTIFICATION_CAPACITY");
    if (chunk.length > bytes.length - used) throw protocol("byte_limit");
    bytes.set(chunk, used);
    used += chunk.length;
  }
  check();
  let body;
  try {
    body = parseJSON(new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(0, used)),
      { rejectDuplicates: true, scalarUnicode: true, maxContainerDepth: 2 });
  } catch { throw protocol("invalid_json"); }
  exactObject(body, ["error", "request_id"]);
  exactObject(body.error, ["code", "message"]);
  if (body.request_id !== id) throw protocol("identity");
  const { code, message } = body.error;
  if (typeof code !== "string" || /^[A-Z0-9_]{1,64}$/.exec(code)?.[0] !== code || typeof message !== "string") throw protocol("invalid_fields");
  const kinds = { 400: "INVALID_REQUEST", 409: "INVALID_REQUEST", 413: "INVALID_REQUEST", 401: "AUTHENTICATION",
    403: "AUTHORITY_REVOKED", 429: "CAPACITY", 503: "SOURCE_UNAVAILABLE" };
  return new NotificationError("NOTIFICATION_" + (kinds[status] ?? "PROTOCOL"), {
    httpStatus: status, requestId: id, diagnostic: Object.freeze({ code, message }), retryAfterMs,
    retryable: (status === 429 && code === "NOTIFICATION_CAPACITY") || (status === 503 && code === "NOTIFICATION_SOURCE_UNAVAILABLE"),
  });
}

module.exports = { streamHeaders, responseError, retryAfter };
