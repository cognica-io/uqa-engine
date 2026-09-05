# UQA for Node.js

`@cognica-io/uqa` provides embedded UQA Engine bindings and direct local or Cloud HTTP SQL for Node.js.

## Install

```sh
npm install @cognica-io/uqa@0.2.0
```

The installation selects one prebuilt native addon from the `@cognica-io` npm organization for the current operating system and CPU. Supported packages cover Linux glibc on x64 and arm64, macOS on x64 and arm64, and Windows MSVC on x64 and arm64.

Applications that use only remote HTTP SQL can omit the optional native packages:

```sh
npm install --omit=optional @cognica-io/uqa@0.2.0
```

`HttpEngine`, `HttpSQLStream`, `SQLParam`, `vector`, and `tensor` run in JavaScript on Node.js 16 or newer using Node's built-in HTTP modules. They require no Rust toolchain, native addon, or embedded database. The package loads the native addon only when embedded `Engine` functionality is used. Both CommonJS and ESM support the main package and the explicit `@cognica-io/uqa/http` entry point.

Save this example in an `.mjs` file so it can use ESM and top-level `await`:

```javascript
import { HttpEngine, SQLParam } from "@cognica-io/uqa/http";

const engine = HttpEngine.fromEnv();
const result = await engine.sql("SELECT $1 AS id", [SQLParam.scalar(42)]);
console.log(result.rows);
```

An explicit URL and token or `fromEnv()` needs no CLI. The asynchronous `local()` and `cloud()` constructors invoke the installed `uqa` CLI once to resolve a project, then send SQL directly over HTTP.

## Use

```javascript
import { Engine } from "@cognica-io/uqa";

const engine = new Engine();
const result = await engine.sql("SELECT 1 AS n");
console.log(result.rows);
engine.close();
```

See the [UQA Engine manual](https://github.com/cognica-io/uqa-engine/blob/v0.2.0/docs/manual/reference/08-bindings-and-extensions.md) for the complete Node.js binding contract.

For existing installations, read the [0.2.0 upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.0/docs/manual/reference/10-upgrading.md).

## License

UQA Engine is licensed under AGPL-3.0-only with optional FOSS and noncommercial application exceptions. See `LICENSING.md` in this package.
