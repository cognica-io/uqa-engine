import type { HttpSQLStreamFrame, JSValue, SQLResult } from "@cognica-io/uqa";
import type { HttpSQLDiagnostic } from "@cognica-io/uqa/http";

export function streamDiagnostic(frame: HttpSQLStreamFrame): HttpSQLDiagnostic | undefined {
  return frame.diagnostic;
}

export function streamRow(frame: HttpSQLStreamFrame): Record<string, JSValue> | undefined {
  return frame.row;
}

export function resultRows(result: SQLResult): Array<Record<string, JSValue>> {
  return result.rows;
}
