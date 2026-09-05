//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { HttpEngineError, invalidResponse } = require("./http-error.js");
const { parseJSON, FloatJSONValue } = require("./http-json.js");
const { cloneScalar, parameterValue, MIN_INT64, MAX_INT64 } = require("./sql-param.js");

function isInt64(value) {
  return (typeof value === "bigint" && value >= MIN_INT64 && value <= MAX_INT64) || Number.isSafeInteger(value);
}

function hexToBytes(value) { return Buffer.from(value, "hex"); }

function encodeStatement(query, params) {
  if (typeof query !== "string" || query.trim() === "") throw new HttpEngineError("SQL text must not be empty");
  if (params !== undefined && params !== null && !Array.isArray(params)) throw new TypeError("SQL parameters must be an array");
  return { sql: query, params: Array.from(params ?? [], encodeParameter) };
}

function encodeParameter(input) {
  const parameter = parameterValue(input);
  if (parameter !== undefined && parameter.kind !== "scalar") return { type: parameter.kind, value: parameter.value };
  const value = parameter === undefined ? cloneScalar(input) : parameter.value;
  if (value === null) return { type: "null" };
  if (typeof value === "boolean") return { type: "boolean", value };
  if (typeof value === "bigint") return { type: "int64", value };
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new HttpEngineError("SQL parameter cannot be represented by the HTTP protocol");
    return { type: Number.isInteger(value) ? "int64" : "float64", value };
  }
  if (typeof value === "string") return { type: "text", value };
  if (value instanceof Uint8Array) return { type: "bytes", hex: Buffer.from(value).toString("hex") };
  return { type: "json", value: encodeJSONValue(value) };
}

function encodeJSONValue(value) {
  if (typeof value === "number" && !Number.isFinite(value)) throw new HttpEngineError("SQL parameter cannot be represented by the HTTP protocol");
  if (value instanceof Uint8Array) return { $uqa_type: "bytes", hex: Buffer.from(value).toString("hex") };
  if (value instanceof Float32Array || value instanceof Float64Array) {
    return Array.from(value, (number) => new FloatJSONValue(encodeJSONValue(number)));
  }
  if (Array.isArray(value)) return value.map(encodeJSONValue);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encodeJSONValue(item)]));
  }
  return value;
}

function decodeRow(row) {
  if (row === null || Array.isArray(row) || typeof row !== "object") throw invalidResponse();
  return Object.fromEntries(Object.entries(row).map(([key, value]) => [key, decodeHTTPValue(value)]));
}

function count(value) {
  if (!Number.isSafeInteger(value) || value < 0) throw invalidResponse();
  return value;
}

function columns(value) {
  if (!Array.isArray(value) || !value.every((column) => typeof column === "string")) throw invalidResponse();
  return value;
}

function decodeResult(result) {
  if (result === null || typeof result !== "object" || !Array.isArray(result.rows)) throw invalidResponse();
  return { columns: columns(result.columns), rows: result.rows.map(decodeRow), affectedRows: count(result.affected_rows) };
}

function decodeHTTPValue(value) {
  if (value instanceof FloatJSONValue) return value.value;
  if (Array.isArray(value)) {
    return value.map(decodeHTTPValue);
  }
  if (typeof value === "bigint") return value >= MIN_INT64 && value <= MAX_INT64 ? value : Number(value);
  if (typeof value === "number") return value;
  if (value === null || typeof value !== "object") {
    return value;
  }
  const kind = value.$uqa_type;
  if (kind === "void" && exactHTTPObject(value, ["$uqa_type"])) return "";
  if (kind === "bytes" && exactHTTPObject(value, ["$uqa_type", "hex"])
      && validHTTPHex(value.hex)) {
    return hexToBytes(value.hex);
  }
  if (kind === "decimal" && canonicalHTTPDecimal(value.value)) {
    return value.value;
  }
  if (kind === "fixed_char" && exactHTTPObject(value, ["$uqa_type", "value"])
      && typeof value.value === "string") {
    return value.value;
  }
  if ((kind === "json" || kind === "jsonb")
      && exactHTTPObject(value, ["$uqa_type", "value"])
      && typeof value.value === "string") {
    try {
      return validateHTTPJSONDocument(parseJSON(value.value));
    } catch {
      throw new HttpEngineError("UQA response body is not valid JSON");
    }
  }
  if (kind === "array" && validHTTPTaggedArrayShape(value) !== undefined) {
    return value.values.map(decodeHTTPValue);
  }
  if (kind === "row" && exactHTTPObject(value, ["$uqa_type", "values"])
      && Array.isArray(value.values)) {
    return value.values.map(decodeHTTPValue);
  }
  if (kind === "record" && exactHTTPObject(value, ["$uqa_type", "fields"])
      && Array.isArray(value.fields)
      && value.fields.every((field) => Array.isArray(field)
        && field.length === 2 && typeof field[0] === "string")) {
    return Object.fromEntries(value.fields.map(([key, item]) => [key, decodeHTTPValue(item)]));
  }
  if (kind === "date" && exactHTTPObject(value, ["$uqa_type", "days"])
      && isHTTPInt32(value.days)) {
    return formatHTTPDate(value.days);
  }
  if (kind === "time" && exactHTTPObject(value, ["$uqa_type", "micros"])
      && isInt64(value.micros)) {
    return formatHTTPTime(value.micros);
  }
  if (kind === "time_tz"
      && exactHTTPObject(value, ["$uqa_type", "micros", "offset_minutes"])
      && isInt64(value.micros) && isHTTPInt32(value.offset_minutes)) {
    return `${formatHTTPTime(value.micros)}${formatHTTPOffset(value.offset_minutes)}`;
  }
  if ((kind === "timestamp" || kind === "timestamp_tz")
      && exactHTTPObject(value, ["$uqa_type", "micros"])
      && isInt64(value.micros)) {
    return formatHTTPTimestamp(value.micros, kind === "timestamp_tz");
  }
  if (kind === "interval"
      && exactHTTPObject(value, ["$uqa_type", "months", "days", "micros"])
      && isHTTPInt32(value.months) && isHTTPInt32(value.days)
      && isInt64(value.micros)) {
    return formatHTTPInterval(value.months, value.days, value.micros);
  }
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, decodeHTTPValue(item)]));
}

function exactHTTPObject(value, keys) {
  const actual = Object.keys(value);
  return actual.length === keys.length && keys.every((key) => Object.prototype.hasOwnProperty.call(value, key));
}

function validHTTPHex(value) {
  return typeof value === "string" && value.length % 2 === 0 && /^[0-9a-f]*$/i.test(value);
}

function canonicalHTTPDecimal(value) {
  return typeof value === "string"
    && /^(?:NaN|Infinity|-Infinity|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?)$/.test(value)
    && !/^-0(?:\.0+)?$/.test(value);
}

function isHTTPInt32(value) {
  return Number.isInteger(value) && value >= -2_147_483_648 && value <= 2_147_483_647;
}

function validHTTPTaggedArrayShape(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)
      || value.$uqa_type !== "array"
      || !exactHTTPObject(value, ["$uqa_type", "lower_bounds", "values"])
      || !Array.isArray(value.lower_bounds)
      || !value.lower_bounds.every(isHTTPInt32)
      || !Array.isArray(value.values)) {
    return undefined;
  }
  const shape = httpArrayShape(value.values);
  if (shape === undefined) {
    return undefined;
  }
  const normalizedShape = shape[0] === 0 ? [] : shape;
  return normalizedShape.length === value.lower_bounds.length ? shape : undefined;
}

function httpArrayShape(values) {
  const dimensions = [values.length];
  let nestedShape;
  let hasScalar = false;
  for (const value of values) {
    const shape = Array.isArray(value)
      ? httpArrayShape(value)
      : validHTTPTaggedArrayShape(value);
    if (shape === undefined) {
      if (nestedShape !== undefined) {
        return undefined;
      }
      hasScalar = true;
      continue;
    }
    if (hasScalar || (nestedShape !== undefined && !sameHTTPShape(nestedShape, shape))) {
      return undefined;
    }
    nestedShape = shape;
  }
  if (nestedShape !== undefined) {
    dimensions.push(...nestedShape);
  }
  return dimensions;
}

function sameHTTPShape(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function validateHTTPJSONDocument(value) {
  if (value instanceof FloatJSONValue) return value.value;
  if (Array.isArray(value)) return value.map(validateHTTPJSONDocument);
  if (typeof value === "bigint") return value >= MIN_INT64 && value <= MAX_INT64 ? value : Number(value);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, validateHTTPJSONDocument(item)]));
  }
  return value;
}

function requireSafeHTTPInteger(value) {
  if (!Number.isSafeInteger(value)) {
    throw invalidResponse();
  }
  return value;
}

function formatHTTPDate(days) {
  requireSafeHTTPInteger(days);
  const date = new Date(days * 86_400_000);
  if (Number.isNaN(date.valueOf())) {
    return String(days);
  }
  const year = date.getUTCFullYear();
  if (year < -262_143 || year > 262_142) {
    return String(days);
  }
  const month = String(date.getUTCMonth() + 1).padStart(2, "0");
  const day = String(date.getUTCDate()).padStart(2, "0");
  return `${formatHTTPYear(year)}-${month}-${day}`;
}

function formatHTTPYear(year) {
  const magnitude = String(Math.abs(year)).padStart(4, "0");
  return year < 0 ? `-${magnitude}` : year > 9_999 ? `+${magnitude}` : magnitude;
}

function formatHTTPTime(source) {
  const day = 86_400_000_000n;
  const micros = ((BigInt(source) % day) + day) % day;
  return formatClock(micros);
}

function formatClock(micros) {
  const hours = micros / 3_600_000_000n;
  const minutes = (micros % 3_600_000_000n) / 60_000_000n;
  const seconds = (micros % 60_000_000n) / 1_000_000n;
  const fraction = micros % 1_000_000n;
  let text = [hours, minutes, seconds].map((value) => String(value).padStart(2, "0")).join(":");
  if (fraction !== 0n) text += "." + String(fraction).padStart(6, "0").replace(/0+$/, "");
  return text;
}

function formatHTTPOffset(source) {
  const minutes = requireSafeHTTPInteger(source);
  const sign = minutes < 0 ? "-" : "+";
  const absolute = Math.abs(minutes);
  return `${sign}${String(Math.floor(absolute / 60)).padStart(2, "0")}:${String(absolute % 60).padStart(2, "0")}`;
}

function formatHTTPTimestamp(source, utc) {
  const micros = BigInt(source);
  const day = 86_400_000_000n;
  const remainder = ((micros % day) + day) % day;
  const days = Number((micros - remainder) / day);
  const date = formatHTTPDate(days);
  if (!/^[+-]?\d{4,}-\d{2}-\d{2}$/.test(date)) return String(micros);
  return date + " " + formatClock(remainder) + (utc ? "+00" : "");
}

function formatHTTPInterval(months, days, source) {
  const micros = BigInt(source);
  const fields = [];
  let negative = false;
  for (const [value, unit] of [[Math.trunc(months / 12), "year"], [months % 12, "mon"], [days, "day"]]) {
    if (value !== 0) {
      fields.push((negative && value > 0 ? "+" : "") + value + " " + unit + (value === 1 ? "" : "s"));
      negative ||= value < 0;
    }
  }
  if (micros !== 0n || fields.length === 0) {
    fields.push((micros < 0n ? "-" : negative ? "+" : "") + formatClock(micros < 0n ? -micros : micros));
  }
  return fields.join(" ");
}

module.exports = { encodeStatement, decodeRow, decodeResult, count, columns };
