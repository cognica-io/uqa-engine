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

function hasIndexedDB() {
  return typeof indexedDB !== "undefined";
}

async function loadModule() {
  if (modulePromise === null) {
    modulePromise = (async () => {
      const module = await createUQAModule();
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
  const text = module.UTF8ToString(ptr);
  module.ccall("uqa_free", null, ["number"], [ptr]);
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

  /** Flush every persistent database to IndexedDB (browser only). */
  async persist() {
    const module = await loadModule();
    if (hasIndexedDB()) {
      await syncFS(module, false);
    }
  },

  /** Directory on the virtual filesystem that persists to IndexedDB. */
  persistDir: PERSIST_DIR,

  async detectDatabaseFile(path) {
    const module = await loadModule();
    return rawCall(module, 0, "detectDatabaseFile", { path });
  },
};

export class Engine {
  constructor(module, handle) {
    this.module = module;
    this.handle = handle;
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
    return rawCall(this.module, this.handle, method, args);
  }

  async sql(query, params) {
    return this.call("sql", { query, params: encodeParams(params) });
  }

  async sqlBatch(statements) {
    return this.call("sqlBatch", {
      statements: statements.map(([sql, params]) => [sql, encodeParams(params) ?? []]),
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

  async hybridSearch(table, textField, textQuery, vectorField, queryVector, topK, knnPool, alpha) {
    return this.call("hybridSearch", {
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
    return this.call("close", {});
  }
}
