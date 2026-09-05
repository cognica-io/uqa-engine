//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { invalidResponse } = require("./http-error.js");

// Preserve integer tokens before JavaScript Number can round them. Strings,
// including escaped keys and type-tag-shaped user data, retain JSON semantics.
function parseJSON(source) {
  let offset = 0;
  function whitespace() {
    while (" \t\r\n".includes(source[offset]) && offset < source.length) offset += 1;
  }
  function string() {
    const start = offset++;
    while (offset < source.length) {
      const character = source[offset++];
      if (character === '"') return JSON.parse(source.slice(start, offset));
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

module.exports = { parseJSON, stringifyJSON, FloatJSONValue };
