//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const parameters = new WeakMap();
const MIN_INT64 = -(1n << 63n);
const MAX_INT64 = (1n << 63n) - 1n;

function cloneScalar(value, ancestors = new Set()) {
  if (value === null || value === undefined) return null;
  if (typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "bigint") {
    if (value < MIN_INT64 || value > MAX_INT64) throw new TypeError("BigInt value is outside i64 range");
    return value;
  }
  if (typeof value === "number") {
    if (Number.isInteger(value) && !Number.isSafeInteger(value)) {
      throw new TypeError("integer-valued JavaScript Number is outside the safe integer range; pass a BigInt");
    }
    return value;
  }
  if (typeof value !== "object") throw new TypeError("unsupported JavaScript SQL value type");
  if (value instanceof Date) throw new TypeError("Date values are not supported; pass an ISO 8601 string instead");
  if (Buffer.isBuffer(value)) return Buffer.from(value);
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof Float32Array) return new Float32Array(value);
  if (value instanceof Float64Array) return new Float64Array(value);
  if (ArrayBuffer.isView(value)) throw new TypeError("unsupported JavaScript typed array");
  if (ancestors.has(value)) throw new TypeError("SQL parameters must not contain cycles");
  ancestors.add(value);
  try {
    if (Array.isArray(value)) return Array.from(value, (item) => cloneScalar(item, ancestors));
    return Object.fromEntries(Object.keys(value).map((key) => [key, cloneScalar(value[key], ancestors)]));
  } finally {
    ancestors.delete(value);
  }
}

function vectorValues(values) {
  if (!(values instanceof Float32Array) && !Array.isArray(values)) {
    throw new TypeError("SQL vector parameter requires a Float32Array or number array");
  }
  return Array.from(values, (value) => {
    if (typeof value !== "number" || !Number.isFinite(value) || Math.abs(value) > 3.4028234663852886e38) {
      throw new TypeError("SQL vector parameter must contain finite f32 values");
    }
    return Math.fround(value);
  });
}

class SQLParam {
  constructor() { throw new TypeError("use SQLParam.scalar, SQLParam.vector, or SQLParam.tensor"); }
  static scalar(value) { return parameter("scalar", cloneScalar(value)); }
  static vector(values) { return parameter("vector", vectorValues(values)); }
  static tensor(values) {
    if (!Array.isArray(values)) throw new TypeError("SQL tensor parameter requires an array of rows");
    return parameter("tensor", values.map(vectorValues));
  }
}

function parameter(kind, value) {
  const result = Object.create(SQLParam.prototype);
  parameters.set(result, { kind, value });
  return result;
}

function parameterValue(value) {
  return parameters.get(value);
}

function nativeParameter(value, binding) {
  const parameter = parameterValue(value);
  return parameter === undefined ? value : binding.SQLParam[parameter.kind](parameter.value);
}

module.exports = { SQLParam, cloneScalar, parameterValue, nativeParameter, MIN_INT64, MAX_INT64 };
