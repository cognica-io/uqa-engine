export {
  HttpEngine, HttpSQLStream, SQLParam, vector, tensor,
  HttpEngineCloudOptions, HttpEngineLocalOptions, HttpSQLBatchExecution,
  HttpSQLExecution, HttpSQLStreamFrame, SQLResult, JSValue, ParamInput,
} from "./index";

export declare class HttpEngineError extends Error {
  readonly code?: string;
  readonly status?: number;
  readonly requestId?: string;
}
