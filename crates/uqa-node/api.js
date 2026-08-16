//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

"use strict";

const binding = require("./index.js");

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

function guardEngineMethods(engineClass) {
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
        return Reflect.apply(method, this, args);
      },
    });
  }
}

function installHTTPStreamIterator(streamClass) {
  Object.defineProperty(streamClass.prototype, Symbol.asyncIterator, {
    configurable: true,
    async *value() {
      for (;;) {
        const frame = await this.nextFrame();
        if (frame === null) {
          return;
        }
        yield frame;
      }
    },
  });
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

guardEngineMethods(binding.Engine);
guardEngineMethods(binding.HttpEngine);
installHTTPStreamIterator(binding.HttpSQLStream);
installRegistrationWrappers(binding.Engine);

module.exports = binding;
module.exports.Engine = binding.Engine;
module.exports.HttpEngine = binding.HttpEngine;
module.exports.HttpSQLStream = binding.HttpSQLStream;
module.exports.SQLParam = binding.SQLParam;
module.exports.detectDatabaseFile = binding.detectDatabaseFile;
module.exports.JSFunctionVolatility = binding.JSFunctionVolatility;
module.exports.migratePythonDB = binding.migratePythonDB;
module.exports.open = binding.open;
module.exports.openAuto = binding.openAuto;
module.exports.openCompressed = binding.openCompressed;
module.exports.openCompressedEncrypted = binding.openCompressedEncrypted;
module.exports.openEncrypted = binding.openEncrypted;
module.exports.tensor = binding.tensor;
module.exports.vector = binding.vector;
