# UQA for Browser WebAssembly

`@cognica-io/uqa-wasm` provides the embedded UQA Engine and direct local or Cloud HTTP SQL for browser applications through WebAssembly.

The published WASM bundle includes the Lucene-compatible Nori dictionary and analyzer. `Engine.sql` can call `list_analyzers()` and `analyze_text('nori', input)` to return Korean token graphs, morphology, offsets, and positions.

## Install

```sh
npm install @cognica-io/uqa-wasm@0.2.3
```

## Use

```javascript
import { Engine, UQA } from "@cognica-io/uqa-wasm";

await UQA.load();
const engine = await Engine.open(`${UQA.persistDir}/notes.uqa`);
const result = await engine.sql("SELECT 1 AS n");
console.log(result.rows);
engine.close();
```

See the [UQA Engine manual](https://github.com/cognica-io/uqa-engine/blob/v0.2.3/docs/manual/reference/08-bindings-and-extensions.md) for the complete Browser WASM binding contract.

For existing installations, read the [0.2.3 upgrade guide](https://github.com/cognica-io/uqa-engine/blob/v0.2.3/docs/manual/reference/10-upgrading.md).

## License

UQA Engine is licensed under AGPL-3.0-only with optional FOSS and noncommercial application exceptions. See `LICENSING.md` in this package.
