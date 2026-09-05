//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const { HttpEngine, HttpEngineError, HttpSQLStream } = require("./http.js");
const { SQLParam, nativeParameter } = require("./sql-param.js");

let binding;

function loadNativeBinding() {
  if (binding === undefined) {
    const candidate = require("./index.js");
    guardEngineMethods(candidate.Engine, candidate);
    installRegistrationWrappers(candidate.Engine);
    binding = candidate;
  }
  return binding;
}

// The exported constructor is inert until embedded functionality is used.
// CommonJS-to-ESM namespace creation can inspect every export without loading an addon.
const Engine = new Proxy(function Engine(...args) {
  if (new.target === undefined) throw new TypeError("Engine requires new");
  const native = loadNativeBinding().Engine;
  return Reflect.construct(native, args, new.target === Engine ? native : new.target);
}, {
  get(target, key, receiver) {
    if (key === Symbol.hasInstance) {
      return (value) => binding !== undefined && value instanceof binding.Engine;
    }
    if (["prototype", "open", "openAuto", "openEncrypted", "openCompressed",
      "openCompressedEncrypted", "detectDatabaseFile"].includes(key)) {
      return Reflect.get(loadNativeBinding().Engine, key);
    }
    return Reflect.get(target, key, receiver);
  },
});

let sqlCallbackDepth = 0;

function runSQLCallback(callback, receiver, args) {
  sqlCallbackDepth += 1;
  try {
    return Reflect.apply(callback, receiver, args);
  } finally {
    sqlCallbackDepth -= 1;
  }
}

function assertEngineCallAllowed() {
  if (sqlCallbackDepth !== 0) {
    throw new Error("Engine methods cannot be called from a JavaScript SQL callback");
  }
}

function guardEngineMethods(engineClass, native) {
  const prototype = engineClass.prototype;
  for (const name of Object.getOwnPropertyNames(prototype)) {
    if (name === "constructor") {
      continue;
    }
    const descriptor = Object.getOwnPropertyDescriptor(prototype, name);
    if (descriptor === undefined || typeof descriptor.value !== "function") {
      continue;
    }
    const method = descriptor.value;
    Object.defineProperty(prototype, name, {
      ...descriptor,
      value(...args) {
        assertEngineCallAllowed();
        if (native !== undefined && (name === "sql" || name === "sqlSync") && args[1] != null) {
          args[1] = args[1].map((value) => nativeParameter(value, native));
        } else if (native !== undefined && (name === "sqlBatch" || name === "sqlBatchSync") && Array.isArray(args[0])) {
          args[0] = args[0].map(([query, params]) => [query, params.map((value) => nativeParameter(value, native))]);
        }
        return Reflect.apply(method, this, args);
      },
    });
  }
}

function wrapAggregateFactory(factory) {
  return function wrappedAggregateFactory() {
    return runSQLCallback(
      () => {
        const state = Reflect.apply(factory, undefined, []);
        if (state === null || typeof state !== "object") {
          return state;
        }
        if (typeof state.then === "function") {
          return state;
        }
        const observe = state.observe ?? state.step;
        const finish = state.finish ?? state.finalize;
        if (typeof observe !== "function" || typeof finish !== "function") {
          return state;
        }
        return {
          observe(...args) {
            return runSQLCallback(observe, state, args);
          },
          finish() {
            return runSQLCallback(finish, state, []);
          },
        };
      },
      undefined,
      [],
    );
  };
}

function installRegistrationWrappers(engineClass) {
  const prototype = engineClass.prototype;
  const registerScalarFunction = prototype.registerScalarFunction;
  const registerTableFunction = prototype.registerTableFunction;
  const registerAggregateFunction = prototype.registerAggregateFunction;

  prototype.registerScalarFunction = function registerScalarFunctionWithBoundary(
    name,
    callback,
    options,
  ) {
    assertEngineCallAllowed();
    if (typeof callback !== "function") {
      throw new TypeError("scalar SQL callback must be a function");
    }
    const wrapped = (...args) => runSQLCallback(callback, undefined, args);
    return Reflect.apply(registerScalarFunction, this, [name, wrapped, options]);
  };

  prototype.registerTableFunction = function registerTableFunctionWithBoundary(
    name,
    callback,
    options,
  ) {
    assertEngineCallAllowed();
    if (typeof callback !== "function") {
      throw new TypeError("table SQL callback must be a function");
    }
    const wrapped = (...args) => runSQLCallback(callback, undefined, args);
    return Reflect.apply(registerTableFunction, this, [name, wrapped, options]);
  };

  prototype.registerAggregateFunction = function registerAggregateFunctionWithBoundary(
    name,
    factory,
    options,
  ) {
    assertEngineCallAllowed();
    if (typeof factory !== "function") {
      throw new TypeError("aggregate SQL callback factory must be a function");
    }
    return Reflect.apply(registerAggregateFunction, this, [
      name,
      wrapAggregateFactory(factory),
      options,
    ]);
  };
}

guardEngineMethods(HttpEngine);

module.exports.Engine = Engine;
module.exports.HttpEngine = HttpEngine;
module.exports.HttpEngineError = HttpEngineError;
module.exports.HttpSQLStream = HttpSQLStream;
module.exports.SQLParam = SQLParam;
module.exports.detectDatabaseFile = (...args) => loadNativeBinding().detectDatabaseFile(...args);
module.exports.JSFunctionVolatility = Object.freeze({ Volatile: "volatile", Stable: "stable", Immutable: "immutable" });
module.exports.migratePythonDB = (...args) => loadNativeBinding().migratePythonDB(...args);
module.exports.open = (...args) => loadNativeBinding().open(...args);
module.exports.openAuto = (...args) => loadNativeBinding().openAuto(...args);
module.exports.openCompressed = (...args) => loadNativeBinding().openCompressed(...args);
module.exports.openCompressedEncrypted = (...args) => loadNativeBinding().openCompressedEncrypted(...args);
module.exports.openEncrypted = (...args) => loadNativeBinding().openEncrypted(...args);
module.exports.tensor = SQLParam.tensor;
module.exports.vector = SQLParam.vector;
