export type JSValue = null | boolean | number | bigint | string | Buffer | Uint8Array | Float32Array | Float64Array | Array<JSValue> | { [key: string]: JSValue }
export type ParamInput = SQLParam | JSValue
export declare class Engine {
  /** Create an in-memory engine. */
  constructor()
  static open(path: string): Engine
  /** Create an independent SQL session over this persistent database. */
  newSession(): Engine
  static openEncrypted(path: string, key: string): Engine
  static openAuto(path: string, key?: string | undefined | null): Engine
  static openCompressed(path: string, options?: CompressionOptions | undefined | null): Engine
  static openCompressedEncrypted(path: string, key: string, options?: CompressionOptions | undefined | null): Engine
  static detectDatabaseFile(path: string): string
  sql(query: string, params?: Array<ParamInput> | undefined | null): Promise<SQLResult>
  sqlSync(query: string, params?: Array<ParamInput> | undefined | null): SQLResult
  sqlBatch(statements: Array<[string, Array<ParamInput>]>): Promise<Array<SQLResult>>
  sqlBatchSync(statements: Array<[string, Array<ParamInput>]>): Array<SQLResult>
  createDefaultTable(name: string, ftsFields: Array<string>): void
  createVectorField(table: string, field: string, dimensions: number): boolean
  addDocument(table: string, docId: number, document: Record<string, JSValue>): void
  addDocumentWithVectors(table: string, docId: number, document: Record<string, JSValue>, vectors: Record<string, Array<number> | Array<Array<number>>>): void
  addVector(table: string, docId: number, field: string, vector: Float32Array | Array<number>): boolean
  addVectorValues(table: string, docId: number, field: string, vectors: Array<Array<number>>): boolean
  getDocument(table: string, docId: number): Record<string, JSValue> | null
  deleteDocument(table: string, docId: number): void
  documentCount(table: string): number
  search(table: string, field: string, query: string, topK?: number | undefined | null, scoring?: string | undefined | null): Promise<Array<SearchHit>>
  searchSync(table: string, field: string, query: string, topK?: number | undefined | null, scoring?: string | undefined | null): Array<SearchHit>
  knnSearch(table: string, field: string, vector: Float32Array | Array<number>, topK?: number | undefined | null): Promise<Array<SearchHit>>
  knnSearchSync(table: string, field: string, vector: Float32Array | Array<number>, topK?: number | undefined | null): Array<SearchHit>
  vectorSimilaritySearch(table: string, field: string, vector: Float32Array | Array<number>, threshold: number): Promise<Array<SearchHit>>
  hybridSearch(table: string, textField: string, textQuery: string, vectorField: string, queryVector: Float32Array | Array<number>, topK?: number | undefined | null, knnPool?: number | undefined | null): Promise<Array<SearchHit>>
  robustHybridSearch(table: string, textField: string, textQuery: string, vectorField: string, queryVector: Float32Array | Array<number>, topK?: number | undefined | null, knnPool?: number | undefined | null, alpha?: number | undefined | null): Promise<Array<SearchHit>>
  estimateScoringParams(table: string, field: string, nSamples?: number | undefined | null, tokensPerQuery?: number | undefined | null, seed?: number | undefined | null): Promise<Record<string, number>>
  learnScoringParams(table: string, field: string, query: string, labels: Array<number>): Promise<Record<string, number>>
  updateScoringParams(table: string, field: string, score: number, label: number): void
  calibrationReport(table: string, field: string, query: string, labels: Array<number>): Promise<CalibrationReport>
  saveScoringParams(name: string, params: Record<string, number>): void
  loadScoringParams(name: string): Record<string, number> | null
  loadAllScoringParams(): Record<string, Record<string, number>>
  dropScoringParams(name: string): boolean
  runCypher(graph: string, query: string, params?: Record<string, JSValue> | undefined | null): Promise<SQLResult>
  runCypherSync(graph: string, query: string, params?: Record<string, JSValue> | undefined | null): SQLResult
  createGraph(name: string): boolean
  dropGraph(name: string): boolean
  listGraphs(): Array<string>
  listPathIndexes(): Array<string>
  tableNames(): Array<string>
  listViews(): Array<string>
  listSchemas(): Array<string>
  listSequences(): Array<string>
  listNamedAnalyzers(): Array<string>
  listForeignServers(): Array<string>
  listForeignTables(): Array<string>
  takeSQLNotices(): Array<SQLNotice>
  sqlFunctionDepthLimit(): number
  setSQLFunctionDepthLimit(limit: number): void
  cancel(): void
  close(): void
}

export declare class SQLParam {
  static scalar(value: unknown): SQLParam
  static vector(values: Float32Array | Array<number>): SQLParam
  static tensor(values: Array<Array<number>>): SQLParam
}

export interface CalibrationReport {
  ece: number
  brier: number
  logLoss: number
  bins: Array<ReliabilityBin>
}

export interface CompressionOptions {
  codec?: string
  pageSize?: number
  chunkPages?: number
  level?: number
}

export declare function detectDatabaseFile(path: string): string

export declare function migratePythonDB(source: string, destination: string): MigrationReport

export interface MigrationReport {
  sourcePath: string
  destinationPath: string
  tables: number
  documents: number
  ftsFields: number
  vectorFields: number
  indexes: number
  analyzers: number
  tableFieldAnalyzers: number
  foreignServers: number
  foreignTables: number
  graphs: number
  graphVertices: number
  graphEdges: number
  pathIndexes: number
  scoringParams: number
  models: number
  columnStats: number
}

export declare function open(path: string): Engine

export declare function openAuto(path: string, key?: string | undefined | null): Engine

export declare function openCompressed(path: string, options?: CompressionOptions | undefined | null): Engine

export declare function openCompressedEncrypted(path: string, key: string, options?: CompressionOptions | undefined | null): Engine

export declare function openEncrypted(path: string, key: string): Engine

export interface ReliabilityBin {
  avgPredicted: number
  avgActual: number
  count: number
}

export interface SearchHit {
  docId: number
  score: number
}

export interface SQLNotice {
  level: string
  message: string
}

export interface SQLResult {
  columns: Array<string>
  rows: Array<Record<string, JSValue>>
  affectedRows: number
}

export declare function tensor(values: Array<Array<number>>): SQLParam

export declare function vector(values: Float32Array | Array<number>): SQLParam
