# HTTP Engine

The `uqa-client` crate provides `HttpEngine`, an asynchronous Rust SQL interface for the HTTP data plane shared by local and Cloud UQA nodes. Native Rust, Python, and Node.js applications can resolve a project name through the installed `uqa` CLI once during construction; every SQL operation after construction goes directly to the data plane over HTTP.

## Install a released binding

UQA-RS release artifacts are attached to the [GitHub release](https://github.com/cognica-io/uqa-engine/releases/tag/v0.1.6). Public Rust crates are published separately to crates.io. Python, Node.js, and Browser WASM remain GitHub-release artifacts only and are not published to PyPI or npm. To use the source from GitHub, pin a Rust application to the same release tag:

```toml
[dependencies]
uqa-client = { git = "https://github.com/cognica-io/uqa-engine", tag = "v0.1.6" }
```

The same version can be taken from the registry as `uqa = "0.1.6"`, `uqa-client = "0.1.6"`, or `uqa-engine = "0.1.6"`.

```sh
python -m pip install ./uqa-0.1.6-cp38-abi3-PLATFORM.whl
npm install ./uqa-0.1.6-PLATFORM.tgz
npm install ./uqa-wasm-0.1.6.tgz
```

The platform-specific Node.js archive includes the native addon; the unqualified `uqa-0.1.6.tgz` archive contains only the JavaScript package and expects a compatible addon package to be installed separately. An application runtime does not need to spawn or bundle the `uqa` CLI when trusted deployment configuration supplies `UQA_URL` and `UQA_TOKEN`.

## Connect by project name

The native Rust client can ask the installed `uqa` CLI to resolve a local project name or a Cloud project name and organization:

```rust
use uqa_client::HttpEngine;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let local = HttpEngine::local("notes").await?;
let cloud = HttpEngine::cloud("analytics", Some("example")).await?;
# let _ = (local, cloud);
# Ok(())
# }
```

`HttpEngine::local` runs `uqa local connection PROJECT --format json`. It uses the local registry and credential store and does not require a Cloud login, but the project node must be ready. `HttpEngine::cloud` runs `uqa cloud connection PROJECT --format json`; it requires a current Cloud login and uses the supplied organization ID or slug, or the CLI's default organization when the argument is `None`.

Both methods resolve `uqa` through `PATH`. Use `HttpEngine::local_with_cli(project, path)` or `HttpEngine::cloud_with_cli(project, organization, path)` when the executable has a fixed nonstandard location. The application process must have access to the same UQA home and native credential store as the interactive CLI user.

The resolver launches the executable directly without a shell, passes no token in arguments, explicitly removes an ambient `UQA_TOKEN` project credential from the child environment, closes stdin, limits stdout and stderr to 64 KiB each, and terminates a lookup after 30 seconds. CLI-specific Cloud and local authentication environment remains available to the child. The resolver accepts only a successful JSON connection response, clears captured credential buffers, discards stderr, and returns redacted errors. Run the matching `uqa ... connection` command directly when a generic lookup failure requires operator diagnostics.

## Connect with deployment configuration

Services that should not invoke a CLI can obtain connection material in a trusted launcher or secret manager. Both connection commands can emit the `UQA_URL` and `UQA_TOKEN` names consumed by `HttpEngine::from_env`:

```sh
uqa local connection notes --format env
uqa cloud connection notes --org example --format env
```

Connection output contains a credential. Never log it, commit it, place it in a command-line argument, or persist it in application configuration. The CLI remains responsible for local project lifecycle, Cloud login and organization selection, project lookup, and native credential-store access.

Applications that already hold connection material can construct the engine explicitly:

```rust
use uqa_client::{HttpEngine, SecretString};

# let project_token = String::from("uqa_db_example");
let engine = HttpEngine::new(
    "https://cognica-project.db.uqa-cloud.cognica.io/",
    SecretString::from(project_token),
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The URL must be one origin with no credentials, path, query, or fragment. Plain HTTP is accepted only for `localhost` or a loopback IP address; every remote endpoint requires HTTPS. Redirects are rejected so authorization cannot cross an origin boundary, and the native client does not route project credentials through process-configured HTTP proxies.

The embedded `Engine` and remote `HttpEngine` implement the common Rust `AsyncSQLEngine` trait for code that only needs `sql` and `sql_batch`. The embedded implementation performs its synchronous work when the future is polled, while the remote implementation awaits network I/O; applications should keep CPU-heavy embedded queries off an asynchronous runtime's I/O worker threads.

## Language bindings

The Python package exposes synchronous project constructors and follows the existing synchronous Python `Engine` shape while releasing the GIL during CLI lookup and HTTP work:

```python
import uqa

local = uqa.HttpEngine.local("notes")
engine = uqa.HttpEngine.cloud("analytics", organization="example")
# A nonstandard installation can pass cli_path="/opt/uqa/bin/uqa".
result, request_id = engine.sql_with_metadata(
    "SELECT id, title FROM notes WHERE id = $1",
    [42],
)
for frame in engine.sql_stream("SELECT id FROM notes ORDER BY id"):
    if frame["type"] == "row":
        print(frame["row"])
```

The Node.js binding exposes asynchronous project constructors and reuses the native Rust HTTP client:

```javascript
const { HttpEngine } = require("uqa");

const local = await HttpEngine.local("notes");
const engine = await HttpEngine.cloud("analytics", { organization: "example" });
// Add cliPath: "/opt/uqa/bin/uqa" when the CLI is outside PATH.
const { result, requestId } = await engine.sqlWithMetadata(
  "SELECT id, title FROM notes WHERE id = $1",
  [42],
);
const stream = await engine.sqlStream("SELECT id FROM notes ORDER BY id");
for await (const frame of stream) {
  if (frame.type === "row") console.log(frame.row);
}
```

The browser package exports a fetch-based `HttpEngine` beside the embedded WASM `Engine`:

```javascript
import { HttpEngine } from "uqa-wasm";

const engine = new HttpEngine(projectURL, projectToken);
const result = await engine.sql(
  "SELECT id, title FROM notes WHERE id = $1",
  [42],
);
for await (const frame of await engine.sqlStream("SELECT id FROM notes ORDER BY id")) {
  if (frame.type === "row") console.log(frame.row);
}
```

Browsers cannot execute a local CLI or read its native credential store, so the browser class intentionally supports only an explicit URL and token or `HttpEngine.fromEnv(environment)`. Browser requests use no cookies and require the data plane to allow `POST` and `OPTIONS`, allow the `authorization` and `content-type` request headers, and expose `x-request-id`. A browser application must keep the project token out of source bundles and durable browser storage; obtain it through a trusted application backend or another short-lived bootstrap path. JavaScript numbers cannot exactly carry integers beyond `Number.MAX_SAFE_INTEGER`, so the browser binding rejects unsafe input and output integers instead of silently rounding them.

## Execute SQL

`HttpEngine::sql` accepts the same query and `SQLParam` slice shape as the embedded `Engine::sql`, but it is asynchronous because it calls `POST /v1/sql`:

```rust
use uqa_client::{HttpEngine, SQLParam};
use uqa_core::Value;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let engine = HttpEngine::from_env()?;
let result = engine
    .sql(
        "SELECT id, title FROM notes WHERE id = $1",
        &[SQLParam::scalar(Value::Int(42))],
    )
    .await?;
assert_eq!(result.rows.len(), 1);
# Ok(())
# }
```

Parameters are encoded by the stable typed data-plane JSON contract. Scalars preserve null, Boolean, signed 64-bit integer, finite float, text, bytes, decimal, temporal, JSON, array, row, record, and map values; vectors, tensors, and every nested scalar container reject non-finite components before any request is sent. Identifiers and SQL fragments cannot be parameters.

`sql` returns the ordinary `uqa_sql::SQLResult`. `sql_with_metadata` returns `SQLExecution`, which dereferences to the same result and also retains the successful `x-request-id` for diagnostics. HTTP responses do not currently carry declared column types or the embedded engine's repeated-label positional carrier, so remote `column_types` entries are unresolved and callers should use unique projection labels.

## Execute an atomic batch

`HttpEngine::sql_batch` accepts the same slice of `(SQL text, parameters)` pairs as the embedded batch method and calls `POST /v1/sql/batch`. The node commits every statement or rolls the whole batch back. `sql_batch_with_metadata` returns `SQLBatchExecution` when the successful request ID is needed.

```rust
# use uqa_client::{HttpEngine, SQLParam};
# use uqa_core::Value;
# async fn example(engine: &HttpEngine) -> Result<(), Box<dyn std::error::Error>> {
let first = [SQLParam::scalar(Value::Int(1))];
let second = [SQLParam::scalar(Value::Int(2))];
engine
    .sql_batch(&[
        ("INSERT INTO items(id) VALUES ($1)", &first),
        ("INSERT INTO items(id) VALUES ($1)", &second),
    ])
    .await?;
# Ok(())
# }
```

An HTTP request does not preserve session state after its response. Use one atomic batch for a multi-statement transaction; long-lived remote sessions and embedded callbacks are not part of this interface.

## Stream rows

`HttpEngine::sql_stream` calls `POST /v1/sql/stream` with `Accept: application/x-ndjson` and returns an incremental `SQLStream`. Call `next_frame` until it returns `None`. A valid sequence begins with `Metadata`, continues with zero or more `Row` frames, and ends with `Complete` or `Error`.

```rust
# use uqa_client::{HttpEngine, SQLStreamFrame};
# fn consume(_: std::collections::BTreeMap<String, uqa_core::Value>) {}
# async fn example(engine: &HttpEngine) -> Result<(), Box<dyn std::error::Error>> {
let mut stream = engine.sql_stream("SELECT id FROM notes ORDER BY id", &[]).await?;
while let Some(frame) = stream.next_frame().await? {
    match frame {
        SQLStreamFrame::Row { row } => consume(row),
        SQLStreamFrame::Error { code, message, request_id } => {
            return Err(format!("{code} ({request_id}): {message}").into());
        }
        SQLStreamFrame::Metadata { .. } | SQLStreamFrame::Complete { .. } => {}
    }
}
# Ok(())
# }
```

The client bounds each NDJSON frame at 64 MiB, rejects invalid frame order, requires a terminal frame, and checks every frame request ID against the HTTP response header.

## Errors and diagnostics

`HttpEngineError` separates CLI availability, timeout, size, exit, and JSON failures from URL, credential, parameter, transport, content-type, response-size, request-identity, stream, and server failures. A Rust server failure retains its HTTP status, stable error code, message, and optional request ID for explicit handling, while `Debug` output redacts CLI diagnostics, server messages, transport URLs, endpoints, credentials, statements, parameters, rows, and streamed values. Python and Node.js surface the redacted display message; browser errors expose only the status, stable code, and request ID. CLI stdout and stderr are bounded at 64 KiB each, materialized JSON bodies at 65 MiB, HTTP error bodies at 64 KiB, and individual stream frames at 64 MiB.

Do not blindly retry SQL mutations. Retry only a bounded transient failure when the operation is known to be safe, and use the preserved request ID when investigating an ambiguous response.
