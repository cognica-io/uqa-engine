//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

export type JSValue =
  | null
  | boolean
  | number
  | string
  | Uint8Array
  | JSValue[]
  | { [key: string]: JSValue };

export type ParamInput = SQLParam | JSValue;

export interface SQLResult {
  columns: string[];
  rows: Array<Record<string, JSValue>>;
  affectedRows: number;
}

export interface SearchHit {
  docId: number;
  score: number;
}

export interface SQLNotice {
  level: string;
  message: string;
}

export interface ReliabilityBin {
  avgPredicted: number;
  avgActual: number;
  count: number;
}

export interface CalibrationReport {
  ece: number;
  brier: number;
  logLoss: number;
  bins: ReliabilityBin[];
}

export interface CompressionOptions {
  codec?: string;
  pageSize?: number;
  chunkPages?: number;
  level?: number;
}

/** Tagged SQL parameter (vector / tensor); scalars pass directly. */
export declare class SQLParam {
  static scalar(value: JSValue | bigint): SQLParam;
  static vector(values: Float32Array | number[]): SQLParam;
  static tensor(values: Array<Float32Array | number[]>): SQLParam;
}

export declare function vector(values: Float32Array | number[]): SQLParam;
export declare function tensor(values: Array<Float32Array | number[]>): SQLParam;

export declare const UQA: {
  /** Preload the WASM module and restore persisted databases. */
  load(): Promise<void>;
  /** Flush every persistent database to IndexedDB; rejects when IndexedDB is unavailable. */
  persist(): Promise<void>;
  /** Directory on the virtual filesystem that persists to IndexedDB. */
  persistDir: string;
  detectDatabaseFile(
    path: string
  ): Promise<"missing" | "sqlite" | "compressed" | "compressed_encrypted" | "unrecognized">;
};

export declare class Engine {
  /** Create an in-memory engine. */
  static inMemory(): Promise<Engine>;
  /**
   * Open a persistent database on the virtual filesystem. Place it
   * under `UQA.persistDir` and call `UQA.persist()` to survive page
   * reloads. Encryption is not available in browser builds.
   */
  static open(path: string): Promise<Engine>;
  static openAuto(path: string): Promise<Engine>;
  static openCompressed(path: string, options?: CompressionOptions): Promise<Engine>;

  /** Create an independent SQL session over the same persistent database. */
  newSession(): Promise<Engine>;

  sql(query: string, params?: ParamInput[]): Promise<SQLResult>;
  sqlBatch(statements: Array<[string, ParamInput[]]>): Promise<SQLResult[]>;
  createDefaultTable(name: string, ftsFields: string[]): Promise<void>;
  createVectorField(table: string, field: string, dimensions: number): Promise<boolean>;
  addDocument(table: string, docId: number, document: Record<string, JSValue | bigint>): Promise<void>;
  addDocumentWithVectors(
    table: string,
    docId: number,
    document: Record<string, JSValue | bigint>,
    vectors: Record<string, number[] | number[][]>
  ): Promise<void>;
  addVector(table: string, docId: number, field: string, vector: Float32Array | number[]): Promise<boolean>;
  addVectorValues(
    table: string,
    docId: number,
    field: string,
    vectors: Array<Float32Array | number[]>
  ): Promise<boolean>;
  getDocument(table: string, docId: number): Promise<Record<string, JSValue> | null>;
  deleteDocument(table: string, docId: number): Promise<void>;
  documentCount(table: string): Promise<number>;
  search(
    table: string,
    field: string,
    query: string,
    topK?: number,
    scoring?: "bm25" | "bayesian"
  ): Promise<SearchHit[]>;
  knnSearch(table: string, field: string, vector: Float32Array | number[], topK?: number): Promise<SearchHit[]>;
  vectorSimilaritySearch(
    table: string,
    field: string,
    vector: Float32Array | number[],
    threshold: number
  ): Promise<SearchHit[]>;
  hybridSearch(
    table: string,
    textField: string,
    textQuery: string,
    vectorField: string,
    queryVector: Float32Array | number[],
    topK?: number,
    knnPool?: number
  ): Promise<SearchHit[]>;
  robustHybridSearch(
    table: string,
    textField: string,
    textQuery: string,
    vectorField: string,
    queryVector: Float32Array | number[],
    topK?: number,
    knnPool?: number,
    alpha?: number
  ): Promise<SearchHit[]>;
  estimateScoringParams(
    table: string,
    field: string,
    nSamples?: number,
    tokensPerQuery?: number,
    seed?: number
  ): Promise<Record<string, number>>;
  learnScoringParams(
    table: string,
    field: string,
    query: string,
    labels: Array<0 | 1>
  ): Promise<Record<string, number>>;
  updateScoringParams(table: string, field: string, score: number, label: 0 | 1): Promise<void>;
  calibrationReport(
    table: string,
    field: string,
    query: string,
    labels: Array<0 | 1>
  ): Promise<CalibrationReport>;
  saveScoringParams(name: string, params: Record<string, number>): Promise<void>;
  loadScoringParams(name: string): Promise<Record<string, number> | null>;
  loadAllScoringParams(): Promise<Record<string, Record<string, number>>>;
  dropScoringParams(name: string): Promise<boolean>;
  runCypher(graph: string, query: string, params?: Record<string, JSValue>): Promise<SQLResult>;
  createGraph(name: string): Promise<boolean>;
  dropGraph(name: string): Promise<boolean>;
  listGraphs(): Promise<string[]>;
  listPathIndexes(): Promise<string[]>;
  tableNames(): Promise<string[]>;
  listViews(): Promise<string[]>;
  listSchemas(): Promise<string[]>;
  listSequences(): Promise<string[]>;
  listNamedAnalyzers(): Promise<string[]>;
  listForeignServers(): Promise<string[]>;
  listForeignTables(): Promise<string[]>;
  takeSQLNotices(): Promise<SQLNotice[]>;
  sqlFunctionDepthLimit(): Promise<number>;
  setSQLFunctionDepthLimit(limit: number): Promise<void>;
  cancel(): Promise<void>;
  close(): Promise<void>;
}
