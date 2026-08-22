# UQA for Node.js

`@cognica-io/uqa` provides embedded UQA Engine bindings and direct local or Cloud HTTP SQL for Node.js.

## Install

```sh
npm install @cognica-io/uqa
```

The installation selects one prebuilt native addon from the `@cognica-io` npm organization for the current operating system and CPU. Supported packages cover Linux glibc on x64 and arm64, macOS on x64 and arm64, and Windows MSVC on x64 and arm64.

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
