# HTTP Engine

The `uqa-client` crate provides `HttpEngine`, an asynchronous Rust SQL interface for the HTTP data plane shared by local and Cloud UQA nodes. Native Rust, Python, and Node.js applications can resolve a project name through the installed `uqa` CLI once during construction; every SQL operation after construction goes directly to the data plane over HTTP.

## Install a released binding

UQA Engine release artifacts are attached to the [GitHub release](https://github.com/cognica-io/uqa-engine/releases/tag/v0.4.9). Public Rust crates are published separately to crates.io, the Python package is published to PyPI as `uqa`, and tagged Node.js and Browser WASM releases are published to npm as `@cognica-io/uqa` and `@cognica-io/uqa-wasm`. The GitHub release notes record the independent registry publication status for the exact version. To use the Rust source from GitHub, pin an application to the same release tag:

```toml
[dependencies]
uqa-client = { git = "https://github.com/cognica-io/uqa-engine", tag = "v0.4.9" }
```

The same version can be taken from the registry as `uqa = "0.4.9"`, `uqa-client = "0.4.9"`, or `uqa-engine = "0.4.9"`.

```sh
python -m pip install uqa==0.4.9
npm install @cognica-io/uqa@0.4.9
npm install @cognica-io/uqa-wasm@0.4.9
```

The small `@cognica-io/uqa` root package contains JavaScript and TypeScript declarations and selects one exact-version native package under `@cognica-io` for Linux glibc x64 or arm64, macOS x64 or arm64, or Windows MSVC x64 or arm64. The same root, platform, and WASM tarballs remain attached to the GitHub release for archive verification. An application runtime does not need to spawn or bundle the `uqa` CLI when trusted deployment configuration supplies `UQA_URL` and `UQA_TOKEN`.

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

The Node.js package implements `HttpEngine` in JavaScript using the built-in `http` and `https` modules on Node.js 16 or newer. The client, SQL parameter helpers, and streaming reader require no Rust toolchain, native addon, or embedded engine. HTTP-only applications can install `@cognica-io/uqa` with `--omit=optional`; both the main package and `@cognica-io/uqa/http` export the same HTTP API for CommonJS and ESM. An explicit URL and token or `fromEnv()` needs no CLI; only `local()` and `cloud()` invoke it for project lookup.

```javascript
const { HttpEngine } = require("@cognica-io/uqa");

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

Node.js preserves signed 64-bit integers through `BigInt` when they exceed the safe `number` range and returns byte values as `Buffer`. Parameter validation, atomic batch submission, response-size bounds, request identities, and terminal stream validation follow the native HTTP contract. Breaking out of a stream's asynchronous iterator closes its response. HTTP requests do not follow redirects or use process-configured HTTP proxies.

The browser package exports a fetch-based `HttpEngine` beside the embedded WASM `Engine`:

```javascript
import { HttpEngine } from "@cognica-io/uqa-wasm";

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

## Rust notification subscriptions

UQA Engine 0.4.9 implements `HttpEngine::subscribe_notifications(&[&str], HttpNotificationOptions).await` and `subscribe_notifications_with_cancellation(..., &NotificationCancellation).await`. They return an owned `HttpNotificationSubscription` only after validating ready for every requested channel. The Client transport is exercised against the actual authenticated Cloud Node endpoint as well as independent protocol peers. The [Cloud server contract](https://github.com/cognica-io/uqa-cloud/blob/main/docs/design/node-notification-sse.md) documents its bounded queues, retained authority, timing policy and local qualification. Select compatible server and SDK artifacts; client support does not establish an installed server capability or qualify a deployed ingress.

The options require positive `max_channels`, `max_queued_events`, `max_queued_bytes`, `max_transport_chunk_bytes`, `connect_timeout`, `ready_timeout` and `max_idle_timeout`. The queue byte limit includes reserved slots and retained string/request-ID allocations. Transport chunk capacity must be at least 65,536 bytes and is separate from the per-event wire bound, parser storage and native HTTP/TLS buffers. Durations are whole milliseconds, no greater than `2^36 - 1` milliseconds and representable by the native monotonic clock; connection timeout cannot exceed readiness timeout. Ready advertises the actual idle timeout, which must satisfy `I > 3H + M` and the caller's idle ceiling. Receiving data after ready does not extend the initial readiness deadline into a total subscription lifetime.

`retry: Some(NotificationRetryOptions { max_attempts, episode_timeout, initial_backoff, max_backoff, max_retry_after })` supplies a bounded policy for post-ready losses; all fields are positive and initial backoff cannot exceed maximum backoff. `None` explicitly disables reconnection. A failed initial attempt returns an error. A post-ready replacement emits `ResyncRequired` with the old identity, then `Reconnected` after the new ready, before replacement notifications. Each new ready ends the previous loss episode. Authentication, protocol, unsupported-capability and local-overflow failures are terminal by default; the client never invokes interactive login or replays SQL.

`next_event().await` returns the existing Core `NotificationEvent` variants with exact `u64` sequences. `initial_ready()` retains original response metadata, and `identity()` advances when the consumer receives Reconnected. A separate worker reads continuously into the bounded queue. Overflow preserves the admitted prefix followed by a typed error; cancellation and observed authority rejection discard unread values. Cancelling a receive future only ends that wait. The monotonic signal returned by `cancellation()` stops connection, registration, reads and retries. `close().await` joins local cleanup and is idempotent; cancelling that future retains its task owner for a repeated close. Drop cancels and aborts the retained task, whose destruction requires continued Tokio runtime progress. Local close does not acknowledge remote listener cleanup.

`HttpNotificationError` exposes `kind()` and a stable content-free `code()`. Explicit accessors inspect protocol/transport/server diagnostics, timeout stage and the original/last failure after reconnect exhaustion. Diagnostic formatting omits credentials, endpoints, channels, payloads, private server messages and unknown codes. The [ownership and preservation proof](../../design/owned-http-notification-subscriptions.md) states the lifecycle, queue and timing assumptions.

The following helper consumes one typed observation and joins local cleanup, preserving an error from that receive:

```rust
use uqa_client::{HttpEngine, notifications::{
    HttpNotificationError, HttpNotificationOptions,
}};
use uqa_core::notifications::NotificationEvent;

async fn receive_once(
    engine: &HttpEngine,
    options: HttpNotificationOptions,
) -> Result<Option<NotificationEvent>, HttpNotificationError> {
    let mut subscription = engine.subscribe_notifications(&["jobs"], options).await?;
    let observation = subscription.next_event().await;
    let cleanup = subscription.close().await;
    match observation {
        Err(error) => Err(error),
        Ok(event) => cleanup.map(|()| event),
    }
}
```

Applications must handle gap and reconnection observations explicitly; they must not treat a replacement epoch as replay. Options come from the application's verified resource and serving-path policy. The [Python binding](08-bindings-and-extensions.md#notification-subscriptions) and the Node.js and browser HTTP adapters below expose the same subscription contract.

## Node.js HTTP notification subscriptions

UQA Engine 0.4.9 exposes `await engine.subscribeNotifications(channels, options)` from both `@cognica-io/uqa` and `@cognica-io/uqa/http`. This HTTP path requires no native addon. Creation returns an `HttpNotificationSubscription` only after validating ready. The actual authenticated Cloud Node and public JavaScript client are tested together; older server releases may not expose this endpoint. Required options are `maxChannels`, `maxQueuedEvents`, `maxQueuedBytes`, `maxTransportChunkBytes`, `connectTimeoutMs`, `readyTimeoutMs` and `maxIdleTimeoutMs`. Supply explicit positive integer limits; transport chunks must allow at least 65,536 bytes, connection timeout cannot exceed readiness timeout, and individual timers cannot exceed Node's 2,147,483,647-millisecond range. Queue bytes charge reference slots and encoded records; they do not claim to bound total process RSS.

Optional `retry` supplies `maxAttempts`, `episodeTimeoutMs`, `initialBackoffMs`, `maxBackoffMs` and `maxRetryAfterMs`, all positive, with initial backoff no greater than maximum backoff. Omission or `null` disables retry. A failed initial registration returns its typed error. Post-ready retries expose `resync_required` with the previous identity and failure code, then `reconnected` with the new identity before replacement data. `epoch` and `requestId` advance when reconnection is consumed. Events are frozen; notification `sequence` is a `bigint`, `processId` is an exact signed 32-bit `number`, and `channel` and `payload` retain their original text. Inapplicable variant fields return `null`.

Use `for await`, `next()` or `nextEvent()` with one pending consumer at a time. A second concurrent receive rejects with `NOTIFICATION_INVALID_REQUEST`. Optional `signal: AbortSignal` cancels actual connection, readiness, reads and retries and exposes `NOTIFICATION_CANCELLED`; `close()` and iterator `return()` join local transport cleanup and end iteration normally. Always close in `finally` when consuming manually. Finalizer cleanup is best effort and has no timing guarantee. Queue overflow preserves its admitted prefix before `NOTIFICATION_BACKPRESSURE`; observed authority rejection and cancellation discard unread values. After observed abort, unread values cannot be returned while cleanup is pending. `isClosed` reports ended delivery; await `close()` to establish local cleanup completion. Ordinary SQL deadlines do not set a subscription's total lifetime.

`NotificationError` exposes a stable content-free `code`, optional `httpStatus`, `requestId`, `timeoutStage`, `reconnectAttempts`, `originalFailure` and `lastAttemptFailure`. `diagnostic` permits explicit inspection of private server details; ordinary error/event inspection excludes payload and private diagnostic text. The [JavaScript HTTP preservation argument](../../design/javascript-http-notifications.md) defines queue accounting, ordered observations, retry and cleanup boundaries. This HTTP client evidence does not establish the embedded Node subscription API or browser delivery.

```javascript
import { HttpEngine } from "@cognica-io/uqa/http";

// options contains the application's explicit capacities and time budgets.
async function receiveOne(url, token, options) {
  const engine = new HttpEngine(url, token);
  const subscription = await engine.subscribeNotifications(["jobs"], options);
  try {
    return await subscription.nextEvent();
  } finally {
    await subscription.close();
  }
}
```

## Browser notification subscriptions

The 0.4.9 browser package exposes `await engine.subscribeNotifications(channels, options)` on `@cognica-io/uqa-wasm`'s `HttpEngine`, returning the exported `HttpNotificationSubscription`. Its options, frozen event fields, exact `bigint` sequence, `NotificationError`, asynchronous iterator and close/AbortSignal behavior follow the [Node HTTP contract](#nodejs-http-notification-subscriptions). The browser uses the same generated decoder, queue and subscription owner. Embedded WASM uses the separate [direct subscription API](08-bindings-and-extensions.md#direct-browser-notification-subscriptions).

The browser sends its bearer header with `fetch`, omits cookies and other ambient credentials, sends no Referer, and never follows a redirect. A cross-origin server must permit the authorization/content-type preflight and expose `X-Request-Id`, `Retry-After` and `Content-Encoding`; CORS permission does not authenticate the request. The serving path must use identity encoding and the specified no-store/no-transform policy. JavaScript can inspect only CORS-visible headers. The Cloud Node implements these headers and has been exercised with the actual browser package. Deployed ingress, TLS and selected-host qualification remain release requirements.

Fetch does not expose a TCP/TLS connection callback: `connectTimeoutMs` bounds the browser request through response headers, including preflight, while `readyTimeoutMs` continues through the complete ready event. A SQL request timeout does not set the subscription's lifetime. Close aborts the owned request, cancels its response reader and releases that reader's lock before completing; it does not own the browser's connection pool or acknowledge a remote cleanup transaction. The [browser preservation proof and verified limits](../../design/browser-notification-fetch.md) distinguish these observable boundaries from platform resource and timing qualification.

## Notification protocol primitives

UQA Engine 0.4.9 exposes the low-level Rust module `uqa_client::notifications` for the [notification protocol](../../design/sql-notifications-and-sse.md). `SubscriptionRequest::new(channels, maximum_channels)` constructs an exact channel set and `encode()` produces its bounded version-one JSON request. `from_json(body, maximum_channels, last_event_id)` validates incoming request bytes, including the depth-two envelope and rejection of nonempty resume headers. The maximum is 65,536 raw bytes for a request or complete SSE block; channel names are exact nonempty UTF-8 strings of at most 63 bytes without NUL or duplicates.

`NotificationDecoder::new(Arc<SubscriptionRequest>, response_request_id, timer_limits)` constructs one response decoder using Core's validated `NotificationRequestId` and explicit `TimerLimits`. `decode(bytes)` returns `DecodeStep { consumed, event }`; retain and resubmit the unconsumed suffix until the supplied bytes are consumed. Each call yields at most one ready, notification, heartbeat or terminal observation. An event can have zero consumed bytes when a preceding exact-limit CR awaits a non-LF lookahead; process the event and resubmit the same suffix. At EOF call `finish()`, and call it again if it returns an event. A missing terminal frame is an error.

The decoder checks identities, closed schemas, exact integer sequences, channel membership, UTF-8, JSON depth and the checked timing relationship before returning an observation. `ready()` exposes the admitted response metadata. Errors latch for that decoder; `ProtocolError` diagnostics retain no rejected input. Remote `ServerFailure::code()` is available explicitly, while diagnostic formatting omits unknown codes, channels and payloads. The [preservation and resource argument](../../design/notification-protocol-decoding.md) states the parser's bounds and assumptions.

Construct and encode a request without performing network I/O:

```rust
# use std::num::NonZeroUsize;
# use uqa_client::notifications::SubscriptionRequest;
# fn example() -> Result<(), Box<dyn std::error::Error>> {
let request = SubscriptionRequest::new(
    &["jobs", "invoices"],
    NonZeroUsize::new(2).unwrap(),
)?;
let body = request.encode()?;
assert_eq!(
    SubscriptionRequest::from_json(&body, NonZeroUsize::new(2).unwrap(), None)?,
    request,
);
# Ok(())
# }
```

These primitives open no connection, establish no listener and select no deployment timing or capacity defaults. The Rust transport above owns connection lifecycle; the authenticated Cloud Node owns server registration and delivery as described in the linked server contract. Existing SQL and NDJSON methods retain their contracts.

## Errors and diagnostics

`HttpEngineError` separates CLI availability, timeout, size, exit, and JSON failures from URL, credential, parameter, transport, content-type, response-size, request-identity, stream, and server failures. A Rust server failure retains its HTTP status, stable error code, message, and optional request ID for explicit handling, while `Debug` output redacts CLI diagnostics, server messages, transport URLs, endpoints, credentials, statements, parameters, rows, and streamed values. Python surfaces the redacted display message. Node.js and browser HTTP errors expose a redacted message plus the status, stable code, and request ID. CLI stdout and stderr are bounded at 64 KiB each, materialized JSON bodies at 65 MiB, HTTP error bodies at 64 KiB, and individual stream frames at 64 MiB.

Do not blindly retry SQL mutations. Retry only a bounded transient failure when the operation is known to be safe, and use the preserved request ID when investigating an ambiguous response.
