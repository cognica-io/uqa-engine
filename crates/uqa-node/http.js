//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { HttpEngineError, invalidResponse } = require("./http-error.js");
const { baseURL, request, requestId, contentType, jsonResponse, succeeded } = require("./http-transport.js");
const { encodeStatement, decodeResult } = require("./http-values.js");
const { resolveProject } = require("./http-cli.js");
const { HttpSQLStream } = require("./http-stream.js");
const { SQLParam } = require("./sql-param.js");

class HttpEngine {
  #url;
  #token;

  constructor(url, token) {
    if (typeof token !== "string" || token.length === 0 || /[\r\n]/.test(token)) {
      throw new HttpEngineError("UQA project token is invalid");
    }
    this.#url = baseURL(url);
    this.#token = token;
  }

  static fromEnv(environment = process.env) {
    for (const key of ["UQA_URL", "UQA_TOKEN"]) {
      if (typeof environment?.[key] !== "string") throw new HttpEngineError("required UQA connection environment variable " + key + " is missing");
    }
    return new HttpEngine(environment.UQA_URL, environment.UQA_TOKEN);
  }

  static async local(project, options) {
    const connection = await resolveProject("local", project, options);
    return new HttpEngine(connection.url, connection.token);
  }

  static async cloud(project, options) {
    const connection = await resolveProject("cloud", project, options);
    return new HttpEngine(connection.url, connection.token);
  }

  async sql(query, params) { return (await this.sqlWithMetadata(query, params)).result; }

  async sqlWithMetadata(query, params) {
    const response = await request(this.#url, this.#token, "v1/sql", encodeStatement(query, params));
    const { body, requestId } = await jsonResponse(response);
    return { result: decodeResult(body), requestId };
  }

  async sqlBatch(statements) { return (await this.sqlBatchWithMetadata(statements)).results; }

  async sqlBatchWithMetadata(statements) {
    if (!Array.isArray(statements)) throw new TypeError("SQL batch statements must be an array");
    const encoded = Array.from(statements, (statement) => {
      if (!Array.isArray(statement) || statement.length !== 2) throw new TypeError("SQL batch statements require query and parameters");
      return encodeStatement(...statement);
    });
    const response = await request(this.#url, this.#token, "v1/sql/batch", { statements: encoded });
    const { body, requestId } = await jsonResponse(response);
    if (!Array.isArray(body.results)) throw invalidResponse();
    return { results: body.results.map(decodeResult), requestId };
  }

  async sqlStream(query, params) {
    const response = await request(this.#url, this.#token, "v1/sql/stream", encodeStatement(query, params), "application/x-ndjson");
    if (!succeeded(response)) await jsonResponse(response);
    contentType(response, "application/x-ndjson");
    return new HttpSQLStream(response, requestId(response));
  }
}

module.exports.HttpEngine = HttpEngine;
module.exports.HttpEngineError = HttpEngineError;
module.exports.HttpSQLStream = HttpSQLStream;
module.exports.SQLParam = SQLParam;
module.exports.vector = SQLParam.vector;
module.exports.tensor = SQLParam.tensor;
