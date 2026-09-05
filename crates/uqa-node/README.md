# UQA for Node.js

`@cognica-io/uqa` provides embedded UQA Engine bindings and direct local or Cloud HTTP SQL for Node.js.

## Install

```sh
npm install @cognica-io/uqa
```

The installation selects one prebuilt native addon from the `@cognica-io` npm organization for the current operating system and CPU. Supported packages cover Linux glibc on x64 and arm64, macOS on x64 and arm64, and Windows MSVC on x64 and arm64.

Applications that use only remote HTTP SQL can omit the optional native packages:

```sh
npm install --omit=optional @cognica-io/uqa
```

`HttpEngine`, `HttpSQLStream`, `SQLParam`, `vector`, and `tensor` run in JavaScript on Node.js 16 or newer using Node's built-in HTTP modules. They require no Rust toolchain, native addon, or embedded database. The package loads the native addon only when embedded `Engine` functionality is used. Both CommonJS and ESM support the main package and the explicit `@cognica-io/uqa/http` entry point.

```javascript
const { HttpEngine, SQLParam } = require("@cognica-io/uqa/http");

const engine = HttpEngine.fromEnv();
const result = await engine.sql("SELECT $1 AS id", [SQLParam.scalar(42)]);
console.log(result.rows);
```

An explicit URL and token or `fromEnv()` needs no CLI. The asynchronous `local()` and `cloud()` constructors invoke the installed `uqa` CLI once to resolve a project, then send SQL directly over HTTP.

## Use

```javascript
const { Engine } = require("@cognica-io/uqa");

const engine = new Engine();
const result = await engine.sql("SELECT 1 AS n");
console.log(result.rows);
engine.close();
```

See the [UQA Engine manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/reference/08-bindings-and-extensions.md) for the complete Node.js binding contract.

## License

UQA Engine is licensed under AGPL-3.0-only with optional FOSS and noncommercial application exceptions. See `LICENSING.md` in this package.
