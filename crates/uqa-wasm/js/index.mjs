//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// TypeScript-facing wrapper over the uqa_call dispatch ABI exported by
// the emscripten module (see ../src/main.rs). Databases live under
// PERSIST_DIR on the emscripten virtual filesystem; in browsers that
// directory is an IDBFS mount, so `UQA.persist()` flushes every
// database into IndexedDB and `UQA.load()` restores them on startup.

import createUQAModule from "./uqa.js";

const PERSIST_DIR = "/uqa";

let modulePromise = null;
const sqlCallbacks = new Map();
let nextSQLCallbackId = 1;
let nextAggregateStateId = 1;
let sqlCallbackDepth = 0;

function hasIndexedDB() {
  return typeof indexedDB !== "undefined";
}

async function loadModule() {
  if (modulePromise === null) {
    modulePromise = (async () => {
      const module = await createUQAModule();
      installCallbackBridge(module);
      module.FS.mkdirTree(PERSIST_DIR);
      if (hasIndexedDB()) {
        module.FS.mount(module.IDBFS, {}, PERSIST_DIR);
        await syncFS(module, true);
      }
      return module;
    })();
  }
  return modulePromise;
}

function installCallbackBridge(module) {
  module.uqaInvokeCallback = (callbackId, requestText) => {
    try {
      const entry = sqlCallbacks.get(callbackId);
      if (entry === undefined) {
        throw new Error(`unknown JavaScript SQL callback ID ${callbackId}`);
      }
      const request = JSON.parse(requestText);
      const result = invokeSQLCallback(entry, request);
      return JSON.stringify({ ok: encodeValue(result) });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return JSON.stringify({ error: message });
    }
  };
}

function invokeSQLCallback(entry, request) {
  sqlCallbackDepth += 1;
  try {
    const args = decodeValue(request.args ?? []);
    switch (request.operation) {
      case "scalar":
        requireCallbackKind(entry, "scalar");
        return synchronousResult(entry.callback(...args), "scalar SQL callback");
      case "table":
        requireCallbackKind(entry, "table");
        return synchronousResult(entry.callback(...args), "table SQL callback");
      case "aggregateCreate":
        requireCallbackKind(entry, "aggregate");
        return createAggregateState(entry);
      case "aggregateObserve":
        requireCallbackKind(entry, "aggregate");
        return observeAggregateState(entry, request.stateId, args);
      case "aggregateFinish":
        requireCallbackKind(entry, "aggregate");
        return finishAggregateState(entry, request.stateId);
      case "aggregateDrop":
        requireCallbackKind(entry, "aggregate");
        entry.states.delete(request.stateId);
        return null;
      default:
        throw new Error(`unknown JavaScript SQL callback operation ${request.operation}`);
    }
  } finally {
    sqlCallbackDepth -= 1;
  }
}

function assertEngineCallAllowed() {
  if (sqlCallbackDepth !== 0) {
    throw new Error("Engine methods cannot be called from a JavaScript SQL callback");
  }
}

function guardEngineMethods(engineClass) {
  const prototype = engineClass.prototype;
  for (const name of Object.getOwnPropertyNames(prototype)) {
    if (name === "constructor") {
      continue;
    }
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (descriptor === undefined || typeof descriptor.value !== "function") {
      continue;
    }
    const method = descriptor.value;
    Object.defineProperty(prototype, name, {
      ...descriptor,
      value(...args) {
        assertEngineCallAllowed();
        return Reflect.apply(method, this, args);
      },
    });
  }
}

function requireCallbackKind(entry, expected) {
  if (entry.kind !== expected) {
    throw new Error(`SQL callback kind mismatch: expected ${expected}, got ${entry.kind}`);
  }
}

function synchronousResult(value, label) {
  if (value !== null && (typeof value === "object" || typeof value === "function")) {
    if (typeof value.then === "function") {
      throw new Error(`${label} must return synchronously; Promise results are not supported`);
    }
  }
  return value === undefined ? null : value;
}

function createAggregateState(entry) {
  const state = synchronousResult(entry.callback(), "SQL aggregate factory");
  if (state === null || typeof state !== "object") {
    throw new Error("SQL aggregate factory must return an object");
  }
  const observe = state.observe ?? state.step;
  const finish = state.finish ?? state.finalize;
  if (typeof observe !== "function") {
    throw new Error("SQL aggregate state needs an observe or step method");
  }
  if (typeof finish !== "function") {
    throw new Error("SQL aggregate state needs a finish or finalize method");
  }
  const stateId = allocateAggregateStateId();
  entry.states.set(stateId, {
    observe: observe.bind(state),
    finish: finish.bind(state),
  });
  return stateId;
}

function aggregateState(entry, stateId) {
  const state = entry.states.get(stateId);
  if (state === undefined) {
    throw new Error(`unknown JavaScript SQL aggregate state ID ${stateId}`);
  }
  return state;
}

function observeAggregateState(entry, stateId, args) {
  const state = aggregateState(entry, stateId);
  synchronousResult(state.observe(...args), "SQL aggregate observe method");
  return null;
}

function finishAggregateState(entry, stateId) {
  const state = aggregateState(entry, stateId);
  try {
    return synchronousResult(state.finish(), "SQL aggregate finish method");
  } finally {
    entry.states.delete(stateId);
  }
}

function allocateAggregateStateId() {
  if (nextAggregateStateId > 0xffffffff) {
    throw new Error("JavaScript SQL aggregate state ID space is exhausted");
  }
  const stateId = nextAggregateStateId;
  nextAggregateStateId += 1;
  return stateId;
}

function createCallbackGroup() {
  return { references: 1, registrations: new Map() };
}

function retainCallbackGroup(group) {
  group.references += 1;
}

function releaseCallbackGroup(group) {
  group.references -= 1;
  if (group.references !== 0) {
    return;
  }
  for (const callbackId of group.registrations.values()) {
    sqlCallbacks.delete(callbackId);
  }
  group.registrations.clear();
}

function registerSQLCallback(group, kind, name, callback, registerNative) {
  if (typeof callback !== "function") {
    throw new TypeError(`${kind} SQL callback must be a function`);
  }
  const normalizedName = String(name).trim().toLowerCase();
  if (normalizedName.length === 0) {
    throw new TypeError("SQL function name cannot be empty");
  }
  if (nextSQLCallbackId > 0xffffffff) {
    throw new Error("JavaScript SQL callback ID space is exhausted");
  }
  const callbackId = nextSQLCallbackId;
  nextSQLCallbackId += 1;
  sqlCallbacks.set(callbackId, {
    kind,
    callback,
    states: kind === "aggregate" ? new Map() : null,
  });
  try {
    registerNative(callbackId);
  } catch (error) {
    sqlCallbacks.delete(callbackId);
    throw error;
  }
  const key = `${kind}:${normalizedName}`;
  const previous = group.registrations.get(key);
  group.registrations.set(key, callbackId);
  if (previous !== undefined) {
    sqlCallbacks.delete(previous);
  }
}

function syncFS(module, populate) {
  return new Promise((resolve, reject) => {
    module.FS.syncfs(populate, (error) => {
      if (error) {
        reject(error);
      } else {
        resolve();
      }
    });
  });
}

function rawCall(module, handle, method, args) {
  const request = JSON.stringify({ method, args });
  const ptr = module.ccall("uqa_call", "number", ["number", "string"], [handle, request]);
  if (ptr === 0) {
    throw new Error("uqa_call could not allocate a response");
  }
  let text;
  try {
    text = module.UTF8ToString(ptr);
  } finally {
    module.ccall("uqa_free", null, ["number"], [ptr]);
  }
  const response = JSON.parse(text);
  if (response.error !== undefined) {
    throw new Error(response.error);
  }
  return decodeValue(response.ok);
}

// {"$bytes": base64} payloads become Uint8Array on the way out;
// Uint8Array/ArrayBuffer arguments become {"$bytes"} on the way in.
function decodeValue(value) {
  if (Array.isArray(value)) {
    return value.map(decodeValue);
  }
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value);
    if (keys.length === 1 && keys[0] === "$bytes") {
      return base64ToBytes(value.$bytes);
    }
    const out = {};
    for (const key of keys) {
      defineOwnValue(out, key, decodeValue(value[key]));
    }
    return out;
  }
  return value;
}

function encodeValue(value) {
  if (value === undefined) {
    return null;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new Error(`non-finite numbers cannot cross the JSON bridge: ${value}`);
    }
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      throw new Error(`integer exceeds JavaScript's safe range: ${value}`);
    }
    return value;
  }
  if (value instanceof Uint8Array) {
    return { $bytes: bytesToBase64(value) };
  }
  if (value instanceof ArrayBuffer) {
    return { $bytes: bytesToBase64(new Uint8Array(value)) };
  }
  if (value instanceof Float32Array || value instanceof Float64Array) {
    return Array.from(value);
  }
  if (typeof value === "bigint") {
    if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < -BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new Error("BigInt values beyond Number.MAX_SAFE_INTEGER are not supported in the browser binding");
    }
    return Number(value);
  }
  if (Array.isArray(value)) {
    return value.map(encodeValue);
  }
  if (value !== null && typeof value === "object") {
    const out = {};
    for (const key of Object.keys(value)) {
      defineOwnValue(out, key, encodeValue(value[key]));
    }
    return out;
  }
  return value;
}

function bytesToBase64(bytes) {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

function base64ToBytes(encoded) {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function encodeParams(params) {
  if (params === undefined || params === null) {
    return undefined;
  }
  return params.map((param) => {
    if (param instanceof SQLParam) {
      return param.payload;
    }
    return encodeValue(param);
  });
}

const MAX_HTTP_JSON_BYTES = 65 * 1024 * 1024;
const MAX_HTTP_ERROR_BYTES = 64 * 1024;
const MAX_HTTP_STREAM_FRAME_BYTES = 64 * 1024 * 1024;

/** Redacted local/Cloud HTTP client error. */
export class HttpEngineError extends Error {
  constructor(message, { code, status, requestId } = {}) {
    super(message);
    this.name = "HttpEngineError";
    this.code = code;
    this.status = status;
    this.requestId = requestId;
  }
}

function httpBaseURL(source) {
  let url;
  try {
    url = new URL(source);
  } catch {
    throw new HttpEngineError("UQA data-plane URL is invalid");
  }
  const exactOrigin = url.username === "" && url.password === "" && url.pathname === "/"
    && url.search === "" && url.hash === "";
  if (!exactOrigin || (url.protocol !== "http:" && url.protocol !== "https:")) {
    throw new HttpEngineError("UQA data-plane URL is invalid");
  }
  if (url.protocol === "http:" && !isLoopbackHostname(url.hostname)) {
    throw new HttpEngineError("plain HTTP UQA URLs must resolve to loopback");
  }
  return url;
}

function isLoopbackHostname(hostname) {
  const normalized = hostname.toLowerCase();
  if (normalized === "localhost" || normalized === "[::1]" || normalized === "::1") {
    return true;
  }
  const octets = normalized.split(".");
  return octets.length === 4
    && octets.every((octet) => /^\d{1,3}$/.test(octet) && Number(octet) <= 255)
    && Number(octets[0]) === 127;
}

function encodeHTTPStatement(query, params) {
  if (typeof query !== "string" || query.trim() === "") {
    throw new HttpEngineError("SQL text must not be empty");
  }
  return {
    sql: query,
    params: (params ?? []).map(encodeHTTPParameter),
  };
}

function encodeHTTPParameter(parameter) {
  if (parameter instanceof SQLParam) {
    if (parameter.httpKind === "bytes") {
      return { type: "bytes", hex: bytesToHex(base64ToBytes(parameter.payload.$bytes)) };
    }
    if (parameter.httpKind === "vector") {
      return { type: "vector", value: finiteHTTPVector(parameter.payload.$vector) };
    }
    if (parameter.httpKind === "tensor") {
      return {
        type: "tensor",
        value: parameter.payload.$tensor.map(finiteHTTPVector),
      };
    }
    return encodeHTTPScalar(parameter.payload);
  }
  return encodeHTTPScalar(parameter);
}

function encodeHTTPScalar(value) {
  if (value === undefined || value === null) {
    return { type: "null" };
  }
  if (typeof value === "boolean") {
    return { type: "boolean", value };
  }
  if (typeof value === "bigint") {
    if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < -BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new HttpEngineError("SQL integer exceeds the browser safe range");
    }
    return { type: "int64", value: Number(value) };
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new HttpEngineError("SQL parameter cannot be represented by the HTTP protocol");
    }
    if (Number.isInteger(value)) {
      if (!Number.isSafeInteger(value)) {
        throw new HttpEngineError("SQL integer exceeds the browser safe range");
      }
      return { type: "int64", value };
    }
    return { type: "float64", value };
  }
  if (typeof value === "string") {
    return { type: "text", value };
  }
  if (value instanceof Uint8Array) {
    return { type: "bytes", hex: bytesToHex(value) };
  }
  if (value instanceof ArrayBuffer) {
    return { type: "bytes", hex: bytesToHex(new Uint8Array(value)) };
  }
  return { type: "json", value: encodeHTTPJSONValue(value) };
}

function encodeHTTPJSONValue(value) {
  if (value === undefined || value === null) {
    return null;
  }
  if (typeof value === "boolean" || typeof value === "string") {
    return value;
  }
  if (typeof value === "bigint") {
    if (value > BigInt(Number.MAX_SAFE_INTEGER) || value < -BigInt(Number.MAX_SAFE_INTEGER)) {
      throw new HttpEngineError("JSON integer exceeds the browser safe range");
    }
    return Number(value);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      throw new HttpEngineError("JSON number cannot be represented by the HTTP protocol");
    }
    return value;
  }
  if (value instanceof Uint8Array) {
    return { $uqa_type: "bytes", hex: bytesToHex(value) };
  }
  if (value instanceof ArrayBuffer) {
    return { $uqa_type: "bytes", hex: bytesToHex(new Uint8Array(value)) };
  }
  if (value instanceof Float32Array || value instanceof Float64Array) {
    return Array.from(value, encodeHTTPJSONValue);
  }
  if (Array.isArray(value)) {
    return value.map(encodeHTTPJSONValue);
  }
  if (typeof value === "object") {
    const encoded = {};
    for (const key of Object.keys(value)) {
      defineOwnValue(encoded, key, encodeHTTPJSONValue(value[key]));
    }
    return encoded;
  }
  throw new HttpEngineError("SQL parameter cannot be represented by the HTTP protocol");
}

function defineOwnValue(object, key, value) {
  Object.defineProperty(object, key, {
    configurable: true,
    enumerable: true,
    value,
    writable: true,
  });
}

function finiteHTTPVector(values) {
  return Array.from(values, (value) => {
    const number = Number(value);
    if (!Number.isFinite(number)) {
      throw new HttpEngineError("SQL parameter cannot be represented by the HTTP protocol");
    }
    return number;
  });
}

function bytesToHex(bytes) {
  let encoded = "";
  for (const byte of bytes) {
    encoded += byte.toString(16).padStart(2, "0");
  }
  return encoded;
}

function hexToBytes(encoded) {
  if (typeof encoded !== "string" || encoded.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(encoded)) {
    throw new HttpEngineError("UQA response body is not valid JSON");
  }
  const bytes = new Uint8Array(encoded.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(encoded.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

function decodeHTTPValue(value) {
  if (Array.isArray(value)) {
    return value.map(decodeHTTPValue);
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      throw new HttpEngineError("UQA response integer exceeds the browser safe range");
    }
    return value;
  }
  if (value === null || typeof value !== "object") {
    return value;
  }
  const kind = value.$uqa_type;
  if (kind === "bytes" && exactHTTPObject(value, ["$uqa_type", "hex"])
      && validHTTPHex(value.hex)) {
    return hexToBytes(value.hex);
  }
  if (kind === "decimal" && canonicalHTTPDecimal(value.value)) {
    return value.value;
  }
  if (kind === "fixed_char" && exactHTTPObject(value, ["$uqa_type", "value"])
      && typeof value.value === "string") {
    return value.value;
  }
  if ((kind === "json" || kind === "jsonb")
      && exactHTTPObject(value, ["$uqa_type", "value"])
      && typeof value.value === "string") {
    try {
      return validateHTTPJSONDocument(JSON.parse(value.value));
    } catch {
      throw new HttpEngineError("UQA response body is not valid JSON");
    }
  }
  if (kind === "array" && validHTTPTaggedArrayShape(value) !== undefined) {
    return value.values.map(decodeHTTPValue);
  }
  if (kind === "row" && exactHTTPObject(value, ["$uqa_type", "values"])
      && Array.isArray(value.values)) {
    return value.values.map(decodeHTTPValue);
  }
  if (kind === "record" && exactHTTPObject(value, ["$uqa_type", "fields"])
      && Array.isArray(value.fields)
      && value.fields.every((field) => Array.isArray(field)
        && field.length === 2 && typeof field[0] === "string")) {
    return Object.fromEntries(value.fields.map(([key, item]) => [key, decodeHTTPValue(item)]));
  }
  if (kind === "date" && exactHTTPObject(value, ["$uqa_type", "days"])
      && isHTTPInt32(value.days)) {
    return formatHTTPDate(value.days);
  }
  if (kind === "time" && exactHTTPObject(value, ["$uqa_type", "micros"])
      && Number.isSafeInteger(value.micros)) {
    return formatHTTPTime(value.micros);
  }
  if (kind === "time_tz"
      && exactHTTPObject(value, ["$uqa_type", "micros", "offset_minutes"])
      && Number.isSafeInteger(value.micros) && isHTTPInt32(value.offset_minutes)) {
    return `${formatHTTPTime(value.micros)}${formatHTTPOffset(value.offset_minutes)}`;
  }
  if ((kind === "timestamp" || kind === "timestamp_tz")
      && exactHTTPObject(value, ["$uqa_type", "micros"])
      && Number.isSafeInteger(value.micros)) {
    return formatHTTPTimestamp(value.micros, kind === "timestamp_tz");
  }
  if (kind === "interval"
      && exactHTTPObject(value, ["$uqa_type", "months", "days", "micros"])
      && isHTTPInt32(value.months) && isHTTPInt32(value.days)
      && Number.isSafeInteger(value.micros)) {
    return formatHTTPInterval(value.months, value.days, value.micros);
  }
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, decodeHTTPValue(item)]));
}

function exactHTTPObject(value, keys) {
  const actual = Object.keys(value);
  return actual.length === keys.length && keys.every((key) => Object.hasOwn(value, key));
}

function validHTTPHex(value) {
  return typeof value === "string" && value.length % 2 === 0 && /^[0-9a-f]*$/i.test(value);
}

function canonicalHTTPDecimal(value) {
  return typeof value === "string"
    && /^(?:NaN|Infinity|-Infinity|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?)$/.test(value)
    && !/^-0(?:\.0+)?$/.test(value);
}

function isHTTPInt32(value) {
  return Number.isInteger(value) && value >= -2_147_483_648 && value <= 2_147_483_647;
}

function validHTTPTaggedArrayShape(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)
      || value.$uqa_type !== "array"
      || !exactHTTPObject(value, ["$uqa_type", "lower_bounds", "values"])
      || !Array.isArray(value.lower_bounds)
      || !value.lower_bounds.every(isHTTPInt32)
      || !Array.isArray(value.values)) {
    return undefined;
  }
  const shape = httpArrayShape(value.values);
  if (shape === undefined) {
    return undefined;
  }
  const normalizedShape = shape[0] === 0 ? [] : shape;
  return normalizedShape.length === value.lower_bounds.length ? shape : undefined;
}

function httpArrayShape(values) {
  const dimensions = [values.length];
  let nestedShape;
  let hasScalar = false;
  for (const value of values) {
    const shape = Array.isArray(value)
      ? httpArrayShape(value)
      : validHTTPTaggedArrayShape(value);
    if (shape === undefined) {
      if (nestedShape !== undefined) {
        return undefined;
      }
      hasScalar = true;
      continue;
    }
    if (hasScalar || (nestedShape !== undefined && !sameHTTPShape(nestedShape, shape))) {
      return undefined;
    }
    nestedShape = shape;
  }
  if (nestedShape !== undefined) {
    dimensions.push(...nestedShape);
  }
  return dimensions;
}

function sameHTTPShape(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function validateHTTPJSONDocument(value) {
  if (Array.isArray(value)) {
    for (const item of value) {
      validateHTTPJSONDocument(item);
    }
    return value;
  }
  if (typeof value === "number") {
    if (!Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value))) {
      throw new HttpEngineError("UQA response integer exceeds the browser safe range");
    }
    return value;
  }
  if (value !== null && typeof value === "object") {
    for (const item of Object.values(value)) {
      validateHTTPJSONDocument(item);
    }
  }
  return value;
}

function requireSafeHTTPInteger(value) {
  if (!Number.isSafeInteger(value)) {
    throw new HttpEngineError("UQA response integer exceeds the browser safe range");
  }
  return value;
}

function formatHTTPDate(days) {
  requireSafeHTTPInteger(days);
  const date = new Date(days * 86_400_000);
  if (Number.isNaN(date.valueOf())) {
    return String(days);
  }
  const year = date.getUTCFullYear();
  if (year < -262_143 || year > 262_142) {
    return String(days);
  }
  const month = String(date.getUTCMonth() + 1).padStart(2, "0");
  const day = String(date.getUTCDate()).padStart(2, "0");
  return `${formatHTTPYear(year)}-${month}-${day}`;
}

function formatHTTPYear(year) {
  const magnitude = String(Math.abs(year)).padStart(4, "0");
  return year < 0 ? `-${magnitude}` : year > 9_999 ? `+${magnitude}` : magnitude;
}

function formatHTTPTime(source) {
  const day = 86_400_000_000;
  const micros = ((requireSafeHTTPInteger(source) % day) + day) % day;
  const hours = Math.floor(micros / 3_600_000_000);
  const minutes = Math.floor((micros % 3_600_000_000) / 60_000_000);
  const seconds = Math.floor((micros % 60_000_000) / 1_000_000);
  const fraction = micros % 1_000_000;
  let output = `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  if (fraction !== 0) {
    output += `.${String(fraction).padStart(6, "0").replace(/0+$/, "")}`;
  }
  return output;
}

function formatHTTPOffset(source) {
  const minutes = requireSafeHTTPInteger(source);
  const sign = minutes < 0 ? "-" : "+";
  const absolute = Math.abs(minutes);
  return `${sign}${String(Math.floor(absolute / 60)).padStart(2, "0")}:${String(absolute % 60).padStart(2, "0")}`;
}

function formatHTTPTimestamp(source, utc) {
  const micros = requireSafeHTTPInteger(source);
  const date = new Date(Math.floor(micros / 1000));
  if (Number.isNaN(date.valueOf())) {
    return String(micros);
  }
  const year = date.getUTCFullYear();
  if (year < -262_143 || year > 262_142) {
    return String(micros);
  }
  const datePart = `${formatHTTPYear(year)}-${String(date.getUTCMonth() + 1).padStart(2, "0")}-${String(date.getUTCDate()).padStart(2, "0")}`;
  const timePart = [date.getUTCHours(), date.getUTCMinutes(), date.getUTCSeconds()]
    .map((value) => String(value).padStart(2, "0"))
    .join(":");
  let output = `${datePart} ${timePart}`;
  const fraction = ((micros % 1_000_000) + 1_000_000) % 1_000_000;
  if (fraction !== 0) {
    output += `.${String(fraction).padStart(6, "0").replace(/0+$/, "")}`;
  }
  return utc ? `${output}+00` : output;
}

function formatHTTPInterval(monthsSource, daysSource, microsSource) {
  const monthsTotal = requireSafeHTTPInteger(monthsSource);
  const days = requireSafeHTTPInteger(daysSource);
  const micros = requireSafeHTTPInteger(microsSource);
  const fields = [];
  let negativeFieldSeen = false;
  const years = Math.trunc(monthsTotal / 12);
  const months = monthsTotal % 12;
  for (const [value, singular] of [[years, "year"], [months, "mon"], [days, "day"]]) {
    if (value !== 0) {
      const sign = negativeFieldSeen && value > 0 ? "+" : "";
      fields.push(`${sign}${value} ${singular}${value === 1 ? "" : "s"}`);
      negativeFieldSeen ||= value < 0;
    }
  }
  if (micros !== 0 || fields.length === 0) {
    const sign = micros < 0 ? "-" : negativeFieldSeen ? "+" : "";
    const absolute = Math.abs(micros);
    const hours = Math.floor(absolute / 3_600_000_000);
    const minutes = Math.floor((absolute % 3_600_000_000) / 60_000_000);
    const seconds = Math.floor((absolute % 60_000_000) / 1_000_000);
    const fraction = absolute % 1_000_000;
    let time = `${sign}${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
    if (fraction !== 0) {
      time += `.${String(fraction).padStart(6, "0").replace(/0+$/, "")}`;
    }
    fields.push(time);
  }
  return fields.join(" ");
}

function validateHTTPContentType(response, expected) {
  const actual = response.headers.get("content-type")?.split(";", 1)[0].trim().toLowerCase();
  if (actual !== expected) {
    throw new HttpEngineError("UQA response content type is invalid");
  }
}

function httpRequestId(response) {
  const requestId = response.headers.get("x-request-id");
  if (requestId === null || requestId === "") {
    throw new HttpEngineError("UQA response is missing its request ID");
  }
  return requestId;
}

async function readBoundedHTTPBody(response, maximumBytes) {
  const declared = response.headers.get("content-length");
  if (declared !== null && Number(declared) > maximumBytes) {
    throw new HttpEngineError("UQA response exceeded the client safety limit");
  }
  if (response.body === null) {
    return new Uint8Array();
  }
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  for (;;) {
    const { value, done } = await readHTTPChunk(reader);
    if (done) {
      break;
    }
    length += value.byteLength;
    if (length > maximumBytes) {
      try {
        await reader.cancel();
      } catch {
        // The bounded error remains the stable public diagnostic.
      }
      throw new HttpEngineError("UQA response exceeded the client safety limit");
    }
    chunks.push(value);
  }
  const body = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    body.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return body;
}

async function readHTTPChunk(reader) {
  try {
    return await reader.read();
  } catch {
    throw new HttpEngineError("UQA HTTP transport failed");
  }
}

async function decodeHTTPJSONResponse(response) {
  const requestId = httpRequestId(response);
  validateHTTPContentType(response, "application/json");
  const maximumBytes = response.ok ? MAX_HTTP_JSON_BYTES : MAX_HTTP_ERROR_BYTES;
  const bytes = await readBoundedHTTPBody(response, maximumBytes);
  let body;
  try {
    body = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new HttpEngineError("UQA response body is not valid JSON");
  }
  if (!response.ok) {
    const code = typeof body?.error?.code === "string" ? body.error.code : "HTTP_ERROR";
    if (body?.request_id !== undefined && body.request_id !== requestId) {
      throw new HttpEngineError("UQA response request IDs do not match");
    }
    throw new HttpEngineError(`UQA returned ${response.status} with code ${code}`, {
      code,
      status: response.status,
      requestId,
    });
  }
  if (body?.request_id !== requestId) {
    throw new HttpEngineError("UQA response request IDs do not match");
  }
  return { body, requestId };
}

function decodeHTTPSQLResult(body) {
  if (!Array.isArray(body?.columns) || !Array.isArray(body?.rows)
      || !body.columns.every((column) => typeof column === "string")
      || !Number.isSafeInteger(body?.affected_rows) || body.affected_rows < 0) {
    throw new HttpEngineError("UQA response body is not valid JSON");
  }
  return {
    columns: body.columns,
    rows: body.rows.map(decodeHTTPRow),
    affectedRows: body.affected_rows,
  };
}

function decodeHTTPRow(row) {
  if (row === null || Array.isArray(row) || typeof row !== "object") {
    throw new HttpEngineError("UQA response body is not valid JSON");
  }
  const decoded = {};
  for (const [key, value] of Object.entries(row)) {
    defineOwnValue(decoded, key, decodeHTTPValue(value));
  }
  return decoded;
}

/** Direct authenticated SQL over the local or Cloud UQA HTTP data plane. */
export class HttpEngine {
  #baseURL;

  #token;

  constructor(url, token) {
    this.#baseURL = httpBaseURL(url);
    if (typeof token !== "string" || token.length === 0) {
      throw new HttpEngineError("UQA project token must not be empty");
    }
    this.#token = token;
  }

  static fromEnv(environment = globalThis.process?.env) {
    if (environment?.UQA_URL === undefined) {
      throw new HttpEngineError("required UQA connection environment variable UQA_URL is missing");
    }
    if (environment?.UQA_TOKEN === undefined) {
      throw new HttpEngineError("required UQA connection environment variable UQA_TOKEN is missing");
    }
    return new HttpEngine(environment.UQA_URL, environment.UQA_TOKEN);
  }

  async #request(path, body, accept = "application/json") {
    if (typeof globalThis.fetch !== "function") {
      throw new HttpEngineError("Fetch API is unavailable in this JavaScript runtime");
    }
    let response;
    try {
      response = await globalThis.fetch(new URL(path, this.#baseURL), {
        method: "POST",
        headers: {
          accept,
          authorization: `Bearer ${this.#token}`,
          "content-type": "application/json",
        },
        body: JSON.stringify(body),
        cache: "no-store",
        credentials: "omit",
        redirect: "error",
        referrerPolicy: "no-referrer",
      });
    } catch {
      throw new HttpEngineError("UQA HTTP transport failed");
    }
    return response;
  }

  async sql(query, params) {
    return (await this.sqlWithMetadata(query, params)).result;
  }

  async sqlWithMetadata(query, params) {
    const response = await this.#request("v1/sql", encodeHTTPStatement(query, params));
    const { body, requestId } = await decodeHTTPJSONResponse(response);
    return { result: decodeHTTPSQLResult(body), requestId };
  }

  async sqlBatch(statements) {
    return (await this.sqlBatchWithMetadata(statements)).results;
  }

  async sqlBatchWithMetadata(statements) {
    const encoded = statements.map(([query, params]) => encodeHTTPStatement(query, params));
    const response = await this.#request("v1/sql/batch", { statements: encoded });
    const { body, requestId } = await decodeHTTPJSONResponse(response);
    if (!Array.isArray(body.results)) {
      throw new HttpEngineError("UQA response body is not valid JSON");
    }
    return { results: body.results.map(decodeHTTPSQLResult), requestId };
  }

  async sqlStream(query, params) {
    const response = await this.#request(
      "v1/sql/stream",
      encodeHTTPStatement(query, params),
      "application/x-ndjson",
    );
    if (!response.ok) {
      await decodeHTTPJSONResponse(response);
    }
    validateHTTPContentType(response, "application/x-ndjson");
    const requestId = httpRequestId(response);
    return new HttpSQLStream(response, requestId);
  }
}

/** Incremental reader for one authenticated UQA NDJSON SQL response. */
export class HttpSQLStream {
  constructor(response, requestId) {
    if (response.body === null) {
      throw new HttpEngineError("UQA NDJSON stream ended before a terminal frame");
    }
    this.reader = response.body.getReader();
    this.requestId = requestId;
    this.chunks = [];
    this.bufferedBytes = 0;
    this.newlineOffset = null;
    this.phase = "metadata";
    this.bodyFinished = false;
  }

  async nextFrame() {
    for (;;) {
      if (this.phase === "finished") {
        return null;
      }
      if (this.phase === "terminal") {
        return this.#finish();
      }
      const bufferedLine = this.#takeLine();
      if (bufferedLine !== null) {
        const line = stripHTTPStreamCR(bufferedLine);
        if (line.byteLength === 0) {
          continue;
        }
        return this.decodeFrame(line);
      }
      if (this.bodyFinished) {
        if (this.bufferedBytes === 0) {
          throw new HttpEngineError("UQA NDJSON stream ended before a terminal frame");
        }
        const line = stripHTTPStreamCR(this.#takeRemainder());
        if (line.byteLength === 0) {
          continue;
        }
        return this.decodeFrame(line);
      }
      await this.#readChunk();
    }
  }

  async #finish() {
    for (;;) {
      const bufferedLine = this.#takeLine();
      if (bufferedLine !== null) {
        if (stripHTTPStreamCR(bufferedLine).byteLength !== 0) {
          throw new HttpEngineError("UQA NDJSON stream frame order is invalid");
        }
        continue;
      }
      if (this.bodyFinished) {
        if (stripHTTPStreamCR(this.#takeRemainder()).byteLength !== 0) {
          throw new HttpEngineError("UQA NDJSON stream frame order is invalid");
        }
        this.phase = "finished";
        return null;
      }
      await this.#readChunk();
    }
  }

  async #readChunk() {
    const { value, done } = await readHTTPChunk(this.reader);
    if (done) {
      this.bodyFinished = true;
      return;
    }
    if (value.byteLength === 0) {
      return;
    }
    if (this.newlineOffset === null) {
      const newline = value.indexOf(10);
      if (newline !== -1) {
        this.newlineOffset = this.bufferedBytes + newline;
      }
    }
    this.chunks.push(value);
    this.bufferedBytes += value.byteLength;
    this.#validateBufferedFrameSize();
  }

  #takeLine() {
    if (this.newlineOffset === null) {
      return null;
    }
    const lineLength = this.newlineOffset;
    if (lineLength > MAX_HTTP_STREAM_FRAME_BYTES) {
      throw new HttpEngineError("UQA NDJSON stream frame exceeded the client safety limit");
    }
    const line = new Uint8Array(lineLength);
    let lineOffset = 0;
    let remaining = lineLength + 1;
    while (remaining !== 0) {
      const chunk = this.chunks.shift();
      const consumed = Math.min(remaining, chunk.byteLength);
      const copied = Math.min(consumed, lineLength - lineOffset);
      if (copied !== 0) {
        line.set(chunk.subarray(0, copied), lineOffset);
        lineOffset += copied;
      }
      if (consumed !== chunk.byteLength) {
        this.chunks.unshift(chunk.subarray(consumed));
      }
      this.bufferedBytes -= consumed;
      remaining -= consumed;
    }
    this.#indexNewline();
    this.#validateBufferedFrameSize();
    return line;
  }

  #takeRemainder() {
    const output = new Uint8Array(this.bufferedBytes);
    let offset = 0;
    for (const chunk of this.chunks) {
      output.set(chunk, offset);
      offset += chunk.byteLength;
    }
    this.chunks = [];
    this.bufferedBytes = 0;
    this.newlineOffset = null;
    return output;
  }

  #indexNewline() {
    this.newlineOffset = null;
    let offset = 0;
    for (const chunk of this.chunks) {
      const newline = chunk.indexOf(10);
      if (newline !== -1) {
        this.newlineOffset = offset + newline;
        return;
      }
      offset += chunk.byteLength;
    }
  }

  #validateBufferedFrameSize() {
    const frameBytes = this.newlineOffset ?? this.bufferedBytes;
    if (frameBytes > MAX_HTTP_STREAM_FRAME_BYTES) {
      throw new HttpEngineError("UQA NDJSON stream frame exceeded the client safety limit");
    }
  }

  decodeFrame(line) {
    let frame;
    try {
      frame = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(line));
    } catch {
      throw new HttpEngineError("UQA response body is not valid JSON");
    }
    if (frame.request_id !== undefined && frame.request_id !== this.requestId) {
      throw new HttpEngineError("UQA NDJSON stream request ID does not match its HTTP response");
    }
    if (this.phase === "metadata" && frame.type === "metadata") {
      if (!Array.isArray(frame.columns)
          || !frame.columns.every((column) => typeof column === "string")
          || !Number.isSafeInteger(frame.row_count) || frame.row_count < 0
          || typeof frame.spilled_to_disk !== "boolean"
          || typeof frame.request_id !== "string" || frame.request_id === "") {
        throw new HttpEngineError("UQA response body is not valid JSON");
      }
      this.phase = "rows";
      return {
        type: "metadata",
        columns: frame.columns,
        rowCount: frame.row_count,
        spilledToDisk: frame.spilled_to_disk,
        requestId: frame.request_id,
      };
    }
    if (this.phase === "rows" && frame.type === "row") {
      return { type: "row", row: decodeHTTPRow(frame.row) };
    }
    if ((this.phase === "metadata" || this.phase === "rows") && frame.type === "error") {
      if (typeof frame.code !== "string" || typeof frame.message !== "string"
          || typeof frame.request_id !== "string" || frame.request_id === "") {
        throw new HttpEngineError("UQA response body is not valid JSON");
      }
      this.phase = "terminal";
      return {
        type: "error",
        code: frame.code,
        message: frame.message,
        requestId: frame.request_id,
      };
    }
    if (this.phase === "rows" && frame.type === "complete") {
      if (!Number.isSafeInteger(frame.row_count) || frame.row_count < 0
          || typeof frame.request_id !== "string" || frame.request_id === "") {
        throw new HttpEngineError("UQA response body is not valid JSON");
      }
      this.phase = "terminal";
      return { type: "complete", rowCount: frame.row_count, requestId: frame.request_id };
    }
    throw new HttpEngineError("UQA NDJSON stream frame order is invalid");
  }

  async *[Symbol.asyncIterator]() {
    for (;;) {
      const frame = await this.nextFrame();
      if (frame === null) {
        return;
      }
      yield frame;
    }
  }
}

function stripHTTPStreamCR(line) {
  return line.at(-1) === 13 ? line.subarray(0, line.byteLength - 1) : line;
}

/** Tagged SQL parameter (vector / tensor); scalars pass directly. */
export class SQLParam {
  constructor(payload, httpKind = "scalar") {
    this.payload = payload;
    Object.defineProperty(this, "httpKind", { value: httpKind });
  }

  static scalar(value) {
    const bytes = value instanceof Uint8Array || value instanceof ArrayBuffer;
    return new SQLParam(encodeValue(value), bytes ? "bytes" : "scalar");
  }

  static vector(values) {
    return new SQLParam({ $vector: Array.from(values) }, "vector");
  }

  static tensor(values) {
    return new SQLParam({ $tensor: values.map((row) => Array.from(row)) }, "tensor");
  }
}

export function vector(values) {
  return SQLParam.vector(values);
}

export function tensor(values) {
  return SQLParam.tensor(values);
}

/** Namespace for module-wide operations. */
export const UQA = {
  /** Preload the WASM module and restore persisted databases. */
  async load() {
    await loadModule();
  },

  /** Flush every persistent database to IndexedDB; rejects when IndexedDB is unavailable. */
  async persist() {
    const module = await loadModule();
    if (!hasIndexedDB()) {
      throw new Error("cannot persist UQA databases because IndexedDB is unavailable");
    }
    await syncFS(module, false);
  },

  /** Directory on the virtual filesystem that persists to IndexedDB. */
  persistDir: PERSIST_DIR,

  async detectDatabaseFile(path) {
    const module = await loadModule();
    return rawCall(module, 0, "detectDatabaseFile", { path });
  },
};

export class Engine {
  constructor(module, handle, callbackGroup = createCallbackGroup()) {
    this.module = module;
    this.handle = handle;
    this.callbackGroup = callbackGroup;
    this.closed = false;
  }

  static async inMemory() {
    const module = await loadModule();
    return new Engine(module, rawCall(module, 0, "new", {}));
  }

  static async open(path) {
    const module = await loadModule();
    return new Engine(module, rawCall(module, 0, "open", { path }));
  }

  static async openAuto(path) {
    const module = await loadModule();
    return new Engine(module, rawCall(module, 0, "openAuto", { path }));
  }

  static async openCompressed(path, options) {
    const module = await loadModule();
    return new Engine(module, rawCall(module, 0, "openCompressed", { path, ...options }));
  }

  call(method, args = {}) {
    if (this.closed) {
      throw new Error("engine is closed");
    }
    return rawCall(this.module, this.handle, method, args);
  }

  async newSession() {
    const handle = this.call("newSession", {});
    retainCallbackGroup(this.callbackGroup);
    return new Engine(this.module, handle, this.callbackGroup);
  }

  async sql(query, params) {
    return this.call("sql", { query, params: encodeParams(params) });
  }

  async sqlBatch(statements) {
    return this.call("sqlBatch", {
      statements: statements.map(([sql, params]) => [sql, encodeParams(params) ?? []]),
    });
  }

  async registerScalarFunction(name, callback, options) {
    registerSQLCallback(this.callbackGroup, "scalar", name, callback, (callbackId) => {
      this.call("registerScalarFunction", { name, callbackId, options });
    });
  }

  async registerTableFunction(name, callback, options) {
    registerSQLCallback(this.callbackGroup, "table", name, callback, (callbackId) => {
      this.call("registerTableFunction", { name, callbackId, options });
    });
  }

  async registerAggregateFunction(name, factory, options) {
    registerSQLCallback(this.callbackGroup, "aggregate", name, factory, (callbackId) => {
      this.call("registerAggregateFunction", { name, callbackId, options });
    });
  }

  async createDefaultTable(name, ftsFields) {
    return this.call("createDefaultTable", { name, ftsFields });
  }

  async createVectorField(table, field, dimensions) {
    return this.call("createVectorField", { table, field, dimensions });
  }

  async addDocument(table, docId, document) {
    return this.call("addDocument", { table, docId, document: encodeValue(document) });
  }

  async addDocumentWithVectors(table, docId, document, vectors) {
    return this.call("addDocumentWithVectors", {
      table,
      docId,
      document: encodeValue(document),
      vectors: encodeValue(vectors),
    });
  }

  async addVector(table, docId, field, vector) {
    return this.call("addVector", { table, docId, field, vector: Array.from(vector) });
  }

  async addVectorValues(table, docId, field, vectors) {
    return this.call("addVectorValues", {
      table,
      docId,
      field,
      vectors: vectors.map((row) => Array.from(row)),
    });
  }

  async getDocument(table, docId) {
    return this.call("getDocument", { table, docId });
  }

  async deleteDocument(table, docId) {
    return this.call("deleteDocument", { table, docId });
  }

  async documentCount(table) {
    return this.call("documentCount", { table });
  }

  async search(table, field, query, topK, scoring) {
    return this.call("search", { table, field, query, topK, scoring });
  }

  async knnSearch(table, field, vector, topK) {
    return this.call("knnSearch", { table, field, vector: Array.from(vector), topK });
  }

  async vectorSimilaritySearch(table, field, vector, threshold) {
    return this.call("vectorSimilaritySearch", {
      table,
      field,
      vector: Array.from(vector),
      threshold,
    });
  }

  async hybridSearch(table, textField, textQuery, vectorField, queryVector, topK, knnPool) {
    return this.call("hybridSearch", {
      table,
      textField,
      textQuery,
      vectorField,
      queryVector: Array.from(queryVector),
      topK,
      knnPool,
    });
  }

  async robustHybridSearch(table, textField, textQuery, vectorField, queryVector, topK, knnPool, alpha) {
    return this.call("robustHybridSearch", {
      table,
      textField,
      textQuery,
      vectorField,
      queryVector: Array.from(queryVector),
      topK,
      knnPool,
      alpha,
    });
  }

  async estimateScoringParams(table, field, nSamples, tokensPerQuery, seed) {
    return this.call("estimateScoringParams", { table, field, nSamples, tokensPerQuery, seed });
  }

  async learnScoringParams(table, field, query, labels) {
    return this.call("learnScoringParams", { table, field, query, labels });
  }

  async updateScoringParams(table, field, score, label) {
    return this.call("updateScoringParams", { table, field, score, label });
  }

  async calibrationReport(table, field, query, labels) {
    return this.call("calibrationReport", { table, field, query, labels });
  }

  async saveScoringParams(name, params) {
    return this.call("saveScoringParams", { name, params });
  }

  async loadScoringParams(name) {
    return this.call("loadScoringParams", { name });
  }

  async loadAllScoringParams() {
    return this.call("loadAllScoringParams", {});
  }

  async dropScoringParams(name) {
    return this.call("dropScoringParams", { name });
  }

  async runCypher(graph, query, params) {
    return this.call("runCypher", { graph, query, params: encodeValue(params ?? null) });
  }

  async createGraph(name) {
    return this.call("createGraph", { name });
  }

  async dropGraph(name) {
    return this.call("dropGraph", { name });
  }

  async listGraphs() {
    return this.call("listGraphs", {});
  }

  async listPathIndexes() {
    return this.call("listPathIndexes", {});
  }

  async tableNames() {
    return this.call("tableNames", {});
  }

  async listViews() {
    return this.call("listViews", {});
  }

  async listSchemas() {
    return this.call("listSchemas", {});
  }

  async listSequences() {
    return this.call("listSequences", {});
  }

  async listNamedAnalyzers() {
    return this.call("listNamedAnalyzers", {});
  }

  async listForeignServers() {
    return this.call("listForeignServers", {});
  }

  async listForeignTables() {
    return this.call("listForeignTables", {});
  }

  async takeSQLNotices() {
    return this.call("takeSQLNotices", {});
  }

  async sqlFunctionDepthLimit() {
    return this.call("sqlFunctionDepthLimit", {});
  }

  async setSQLFunctionDepthLimit(limit) {
    return this.call("setSQLFunctionDepthLimit", { limit });
  }

  async cancel() {
    return this.call("cancel", {});
  }

  async close() {
    if (this.closed) {
      return;
    }
    this.call("close", {});
    this.closed = true;
    releaseCallbackGroup(this.callbackGroup);
  }
}

guardEngineMethods(Engine);
