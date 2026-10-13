export {
  HttpEngine, HttpSQLStream, SQLParam, vector, tensor,
  HttpEngineCloudOptions, HttpEngineLocalOptions, HttpSQLBatchExecution,
  HttpSQLExecution, HttpSQLStreamFrame, SQLResult, JSValue, ParamInput,
} from "./index";
export * from "./notifications";

export interface HttpLimit {
  readonly kind: "request_body_bytes" | "sql_bytes" | "parameters" | "batch_statements" |
    "result_bytes" | "query_admission" | "authentication_admission";
  readonly limit: number;
  readonly actual: number;
  readonly actualIsLowerBound: boolean;
  readonly executingLimit?: number;
}

export declare class HttpEngineError extends Error {
  readonly code?: string;
  readonly status?: number;
  readonly limit?: HttpLimit;
  readonly requestId?: string;
  readonly diagnostic?: HttpSQLDiagnostic;
}

export interface HttpSQLDiagnostic {
  readonly sqlstate?: string;
  readonly category: "syntax" | "undefined_table" | "undefined_column" | "ambiguous_column" |
    "undefined_function" | "ambiguous_function" | "type_mismatch" | "invalid_parameter" |
    "vector_dimension_mismatch" | "index_required" | "undefined_object" | "duplicate_object" |
    "constraint_violation" | "unsupported" | "cancelled" | "resource_exhausted" |
    "transaction" | "permission" | "internal" | "other";
  /** Zero-based index into the submitted batch. */
  readonly statementIndex?: number;
  /** One-based character position in the failing statement. */
  readonly position?: number;
}
