//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

class HttpEngineError extends Error {
  constructor(message, { code, status, requestId } = {}) {
    super(message);
    this.name = "HttpEngineError";
    this.code = code;
    this.status = status;
    this.requestId = requestId;
  }
}

function invalidResponse() {
  return new HttpEngineError("UQA response body is not valid JSON");
}

module.exports = { HttpEngineError, invalidResponse };
