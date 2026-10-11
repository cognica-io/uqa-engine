//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const categories = new Set([
  "syntax", "undefined_table", "undefined_column", "ambiguous_column",
  "undefined_function", "ambiguous_function", "type_mismatch", "invalid_parameter",
  "vector_dimension_mismatch", "index_required", "undefined_object", "duplicate_object",
  "constraint_violation", "unsupported", "cancelled", "resource_exhausted",
  "transaction", "permission", "internal", "other",
]);

// Copy only closed categories, a five-character SQLSTATE and bounded numeric positions.
// Error messages, hints, SQL and identifiers from a remote peer never enter this object.
function sqlDiagnostic(code, value) {
  if (code !== "SQL_EXECUTION_FAILED" || value == null || typeof value !== "object" ||
      Array.isArray(value) || !categories.has(value.category) ||
      Object.keys(value).some((key) => !["sqlstate", "category", "statement_index", "position"].includes(key))) return undefined;
  if (value.sqlstate != null && (typeof value.sqlstate !== "string" ||
      !/^[A-Z0-9]{5}$/.test(value.sqlstate) || value.sqlstate === "00000")) return undefined;
  for (const field of ["statement_index", "position"]) {
    if (value[field] != null && (!Number.isInteger(value[field]) || value[field] < 0 ||
        value[field] > 0xffffffff || (field === "position" && value[field] === 0))) return undefined;
  }
  return Object.freeze({
    category: value.category,
    ...(value.sqlstate == null ? {} : { sqlstate: value.sqlstate }),
    ...(value.statement_index == null ? {} : { statementIndex: value.statement_index }),
    ...(value.position == null ? {} : { position: value.position }),
  });
}

function diagnosticMessage(value) {
  if (value === undefined) return "";
  let text = value.category;
  if (value.sqlstate !== undefined) text += "; SQLSTATE " + value.sqlstate;
  if (value.statementIndex !== undefined) text += "; batch statement " + (value.statementIndex + 1) + " (index " + value.statementIndex + ")";
  if (value.position !== undefined) text += "; character " + value.position;
  return " (" + text + ")";
}

module.exports = { sqlDiagnostic, diagnosticMessage };
