//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { invalidResponse } = require("./http-error.js");

// Preserve integer tokens before JavaScript Number can round them. Strings,
// including escaped keys and type-tag-shaped user data, retain JSON semantics.
function parseJSON(source, { rejectDuplicates = false, scalarUnicode = false, maxContainerDepth = Infinity } = {}) {
  let offset = 0;
  function whitespace() {
    while (" \t\r\n".includes(source[offset]) && offset < source.length) offset += 1;
  }
  function string() {
    const start = offset++;
    while (offset < source.length) {
      const character = source[offset++];
      if (character === '"') {
        const result = JSON.parse(source.slice(start, offset));
        if (scalarUnicode && !validUnicode(result)) throw invalidResponse();
        return result;
      }
      if (character === "\\") offset += 1;
    }
    throw invalidResponse();
  }
  function value(depth) {
    if (depth > 128) throw invalidResponse();
    whitespace();
    const character = source[offset];
    if (character === '"') return string();
    if (character === "[" || character === "{") {
      if (depth + 1 > maxContainerDepth) throw invalidResponse();
      const array = character === "[";
      const output = array ? [] : {};
      const end = array ? "]" : "}";
      offset += 1;
      whitespace();
      if (source[offset] === end) { offset += 1; return output; }
      for (;;) {
        whitespace();
        let key;
        if (!array) {
          if (source[offset] !== '"') throw invalidResponse();
          key = string();
          if (rejectDuplicates && Object.prototype.hasOwnProperty.call(output, key)) throw invalidResponse();
          whitespace();
          if (source[offset++] !== ":") throw invalidResponse();
        }
        const item = value(depth + 1);
        if (array) output.push(item);
        else Object.defineProperty(output, key, {
          value: item, enumerable: true, configurable: true, writable: true,
        });
        whitespace();
        if (source[offset] === end) { offset += 1; return output; }
        if (source[offset++] !== ",") throw invalidResponse();
      }
    }
    for (const [literal, result] of [["true", true], ["false", false], ["null", null]]) {
      if (source.startsWith(literal, offset)) { offset += literal.length; return result; }
    }
    const match = /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(source.slice(offset));
    if (match === null) throw invalidResponse();
    offset += match[0].length;
    const number = Number(match[0]);
    if (!Number.isFinite(number)) throw invalidResponse();
    if (/[.eE]/.test(match[0])) return new FloatJSONValue(number);
    if (!Number.isSafeInteger(number)) return BigInt(match[0]);
    return number;
  }
  try {
    const output = value(0);
    whitespace();
    if (offset !== source.length) throw invalidResponse();
    return output;
  } catch {
    throw invalidResponse();
  }
}

function validUnicode(value) {
  if (typeof value !== "string") return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(++index);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return false;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return true;
}

// Typed floating-point arrays retain integral float tokens in JSON parameters.
class FloatJSONValue {
  constructor(value) { this.value = value; }
}

function stringifyJSON(value) {
  if (value instanceof FloatJSONValue) {
    const text = String(value.value);
    return /[.eE]/.test(text) ? text : text + ".0";
  }
  if (typeof value === "bigint") return value.toString();
  if (Array.isArray(value)) return "[" + value.map(stringifyJSON).join(",") + "]";
  if (value !== null && typeof value === "object") {
    return "{" + Object.entries(value).map(([key, item]) =>
      JSON.stringify(key) + ":" + stringifyJSON(item)).join(",") + "}";
  }
  return JSON.stringify(value);
}

module.exports = { parseJSON, stringifyJSON, FloatJSONValue, validUnicode };
