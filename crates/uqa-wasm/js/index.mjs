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
      out[key] = decodeValue(value[key]);
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
      out[key] = encodeValue(value[key]);
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

/** Tagged SQL parameter (vector / tensor); scalars pass directly. */
export class SQLParam {
  constructor(payload) {
    this.payload = payload;
  }

  static scalar(value) {
    return new SQLParam(encodeValue(value));
  }

  static vector(values) {
    return new SQLParam({ $vector: Array.from(values) });
  }

  static tensor(values) {
    return new SQLParam({ $tensor: values.map((row) => Array.from(row)) });
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
