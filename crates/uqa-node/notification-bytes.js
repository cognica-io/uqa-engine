//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

// Count without allocating a temporary encoded payload before queue admission.
// TextEncoder replaces isolated UTF-16 surrogates with the three-byte U+FFFD.
function byteLength(text) {
  let bytes = 0;
  for (let index = 0; index < text.length; index += 1) {
    const code = text.charCodeAt(index);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff &&
        text.charCodeAt(index + 1) >= 0xdc00 && text.charCodeAt(index + 1) <= 0xdfff) {
      bytes += 4;
      index += 1;
    } else bytes += 3;
  }
  return bytes;
}

function encode(text) { return encoder.encode(text); }
function encodeInto(text, destination) { return encoder.encodeInto(text, destination).written; }
function decode(bytes) { return decoder.decode(bytes); }

function compare(left, right) {
  for (let index = 0, length = Math.min(left.length, right.length); index < length; index += 1) {
    if (left[index] !== right[index]) return left[index] - right[index];
  }
  return left.length - right.length;
}

module.exports = { byteLength, encode, encodeInto, decode, compare };
