//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const kinds = new Map([
  ["request_body_bytes", "REQUEST_BODY_TOO_LARGE"], ["sql_bytes", "SQL_TOO_LARGE"],
  ["parameters", "TOO_MANY_PARAMETERS"], ["batch_statements", "TOO_MANY_STATEMENTS"],
  ["result_bytes", "RESULT_TOO_LARGE"], ["query_admission", "NODE_BUSY"],
  ["authentication_admission", "NODE_BUSY"],
]);

function httpLimit(code, value) {
  if (value == null || typeof value !== "object" || Array.isArray(value) ||
      !kinds.has(value.kind) || kinds.get(value.kind) !== code ||
      Object.keys(value).some((key) => !["kind", "limit", "actual", "actual_is_lower_bound", "executing_limit"].includes(key)) ||
      !Number.isSafeInteger(value.limit) || !Number.isSafeInteger(value.actual) ||
      value.limit < 0 || value.actual <= value.limit ||
      (value.kind !== "parameters" && value.limit === 0) ||
      typeof value.actual_is_lower_bound !== "boolean") return undefined;
  const admission = value.kind === "query_admission" || value.kind === "authentication_admission";
  if (admission ? (!value.actual_is_lower_bound || !Number.isSafeInteger(value.executing_limit) || value.executing_limit <= 0) :
      (value.executing_limit != null || value.actual_is_lower_bound !== (value.kind === "request_body_bytes"))) return undefined;
  return Object.freeze({ kind: value.kind, limit: value.limit, actual: value.actual,
    actualIsLowerBound: value.actual_is_lower_bound,
    ...(admission ? { executingLimit: value.executing_limit } : {}),
  });
}

function limitMessage(value) {
  if (value === undefined) return "";
  return " (" + value.kind + " limit " + value.limit + "; observed " +
    (value.actualIsLowerBound ? "at least " : "") + value.actual +
    (value.executingLimit === undefined ? "" : "; executing limit " + value.executingLimit) + ")";
}

module.exports = { httpLimit, limitMessage };
