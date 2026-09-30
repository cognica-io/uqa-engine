# Rust Engine API

The `uqa` facade is the primary application dependency and re-exports the `uqa-engine` crate documented here. `uqa-engine` owns durable storage, session-local SQL state, runtime extensions, epochs, and query execution and remains available as a direct dependency.

## Construct an engine

| API | Result |
| --- | --- |
| `Engine::new()` | In-memory engine |
| `Engine::open(path)` | Default persistent SQLite engine |
| `Engine::detect_database_file(path)` | File format detection without opening the engine |
| `Engine::open_auto(path, key)` | Detect plain, encrypted, compressed, or compressed-encrypted SQLite formats |
| `Engine::open_encrypted(path, key)` | SQLCipher-backed persistent engine |
| `Engine::open_compressed(path, options)` | Compressed SQLite container |
| `Engine::open_compressed_encrypted(path, key, options)` | Compressed and encrypted container |
| `Engine::open_compressed_encrypted_with_anchor(...)` | Compressed and encrypted container with a trusted rollback anchor |
| `Engine::from_persistent_provider(provider)` | Engine over a `PersistentStorageProvider`, including redb |

Use `Engine::close()` when the application needs an explicit close boundary. Normal Rust ownership still closes resources when the engine is dropped.

## Execute SQL

The central method is:

```rust
pub fn sql(
    &self,
    query: &str,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError>
```

`SQLParam` accepts scalar values and also has explicit vector and tensor constructors. Positional placeholders are `$1`, `$2`, and so on.

```rust
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam};

let engine = Engine::new();
engine.sql(
    "CREATE TABLE items (id INTEGER PRIMARY KEY, embedding VECTOR(3))",
    &[],
)?;
engine.sql(
    "INSERT INTO items (id, embedding) VALUES ($1, $2)",
    &[
        SQLParam::scalar(Value::Int(1)),
        SQLParam::vector(vec![1.0, 0.0, 0.0]),
    ],
)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`SQLResult` contains:

- `columns`: projected column labels in order
- `column_types`: SQL types in projection order, including empty results
- `rows`: compatibility rows represented as `BTreeMap<String, Value>`
- `value_at(row, column)`: positional access that distinguishes repeated labels
- `affected_rows`: the DML row count
- `command_tag`: optional PostgreSQL command completion text, such as `SELECT 2`, `INSERT 0 1`, or `CREATE TABLE`
- `kind`: `SQLResultKind::Command` or `Rows`, distinguishing commands from zero-column queries; clients without descriptor information use `Unknown`

When projected labels repeat, `SQLResult` retains the distinct final values in a positional carrier while keeping `rows` for existing named-map callers. Use `value_at`, a cursor, or the columnar path to address repeated labels by position.

`REGTYPE` results, including `pg_typeof`, use `Value::Int` for the catalog OID. Request `pg_typeof(expression)::text` for the visible type name, or use `sql::format_postgres_text(value, column_type, Some(&engine))` with the result's declared type. `sql::postgres_result_type` returns the PostgreSQL type OID, size, and modifier; scalar domain results expose their base type in a PostgreSQL client descriptor.

### Enum labels in results

Enum results arrive as `Value::Enum`, which carries the type OID and an immutable label key rather than the label text, because a label can be renamed without rewriting stored values. `sql::format_postgres_text(value, column_type, Some(&engine))` renders the current label, including inside arrays and records. `Engine::render_enum_labels(&mut result)` replaces every enum value of a result by its label while keeping the declared column types, as a PostgreSQL client receives enum values; it reports an error if a value's type has since been dropped. `Engine::sql_batch_with_labels` runs a batch like `Engine::sql_batch` and renders each result before the next statement runs, so a later statement that drops the type cannot invalidate an earlier result. The Python, Node.js and browser bindings, the Arrow and Parquet `QueryBuilder` outputs, and arguments passed to registered host functions use these labels.

```rust
let engine = uqa_engine::Engine::new();
engine.sql("CREATE TYPE mood AS ENUM ('sad', 'happy')", &[])?;
let mut result = engine.sql("SELECT 'happy'::mood AS m", &[])?;
engine.render_enum_labels(&mut result)?;
assert_eq!(result.value_at(0, 0), Some(&uqa_core::Value::Str("happy".into())));
# Ok::<(), uqa_engine::SQLError>(())
```

### Simple Query messages

`Engine::sql_simple_query(query, params, consume)` accepts a SQL message containing zero or more statements and a `FnMut(&SQLResult) -> Result<(), SQLError>` callback. It parses the complete message before executing any statement, then delivers results in statement order. An empty message delivers one empty result with no `command_tag`.

Statements outside explicit transaction blocks share an implicit transaction segment. Explicit transaction commands control the segment boundaries. The final result is delivered after its implicit transaction commits, so a deferred constraint failure prevents that final callback. Earlier callbacks may already have observed results when a later statement fails; those earlier results do not establish that the segment committed. An execution error stops the remaining statements, and a callback error rolls back an implicit segment that is still open. A final callback runs after commit and cannot undo that commit.

```rust
let engine = uqa_engine::Engine::new();
let mut tags = Vec::new();
engine.sql_simple_query("SELECT 1; SELECT 2", &[], |result| {
    tags.push(result.command_tag.clone());
    Ok(())
})?;
assert_eq!(tags, [Some("SELECT 1".into()), Some("SELECT 1".into())]);
# Ok::<(), uqa_engine::SQLError>(())
```

## Stream results

`Engine::sql_cursor` accepts one read query and returns a row cursor. The engine uses bounded spill when needed, and the read snapshot is committed before the cursor is returned. This makes the cursor suitable for large results without holding an open storage transaction for the consumer lifetime.

`Engine::sql_columnar` consumes result batches through a callback. It is the preferred path for column-oriented consumers and export code.

```mermaid
flowchart TD
    A[SQL text and parameters] --> B{Consumer shape}
    B -->|Materialized result| C[Engine::sql]
    B -->|Row stream| D[Engine::sql_cursor]
    B -->|Column batches| E[Engine::sql_columnar]
```

## COPY streams

`Engine::copy_from(statement, reader)` consumes a `COPY relation [(column, ...)] FROM STDIN` text or CSV stream and returns the inserted row count. `Engine::copy_to(statement, writer)` emits `COPY relation [(column, ...)] TO STDOUT` or `COPY (query) TO STDOUT` and returns the emitted row count. COPY options are parsed with the PostgreSQL 18 grammar; the embedded stream endpoints implement text and CSV with `DELIMITER`, `NULL`, `HEADER`, `QUOTE`, `ESCAPE`, and UTF-8 `ENCODING`.

```rust
let engine = uqa_engine::Engine::new();
engine.sql(
    "CREATE TABLE events (id INTEGER, payload TEXT DEFAULT 'pending')",
    &[],
)?;
engine.copy_from(
    "COPY events (id) FROM STDIN",
    b"1\n2\n".as_slice(),
)?;
let mut output = Vec::new();
engine.copy_to(
    "COPY (SELECT id, payload FROM events ORDER BY id) TO STDOUT",
    &mut output,
)?;
assert_eq!(output, b"1\tpending\n2\tpending\n");
# Ok::<(), Box<dyn std::error::Error>>(())
```

`COPY FROM` uses the ordinary INSERT path as one statement: declarative partition parents route each row after defaults, identity allocation, and stored generation; a direct partition validates every ancestor bound; ordinary inheritance writes only the named relation; and any format, conversion, or constraint error publishes no rows. Direct `COPY relation TO` reads only the named physical relation and omits generated columns, so a partitioned parent raises `42809`; use `COPY (SELECT ... FROM parent) TO STDOUT` to include descendants or put `ONLY` inside that query to exclude them. A generated column named in a direct COPY column list is `42P10`, duplicate and missing names are `42701` and `42703`, malformed row widths are `22P04`, invalid text conversion uses the target type's PostgreSQL SQLSTATE, and a COPY failure aborts the current explicit transaction.

## Transactions and batches

The engine exposes explicit transaction primitives:

- `begin`, `commit`, and `rollback`
- `savepoint`, `release_savepoint`, and `rollback_to_savepoint`
- `transaction_failed` and `pending_transaction_completion` for failed or unresolved transaction state; `pending_commit` exposes only a physical write identity
- SQL forms such as `BEGIN`, `SAVEPOINT`, and `COMMIT`

`Engine::transaction` executes a Rust closure as one transaction. An error or panic from the closure rolls the transaction back; a successful closure attempts to commit it.

Development versioned provider sessions retain uncertain completion. A commit or rollback whose durable result cannot be confirmed returns SQLSTATE `08007` and leaves `Engine::pending_transaction_completion()` set to `TransactionOutcomeId::Records` for a physical publication or `TransactionOutcomeId::Serializable` for logical SSI completion without a write receipt. `pending_commit()` continues to expose only a physical write identity; its absence does not establish that logical completion finished. Ordinary SQL, nested BEGIN and savepoint commands are blocked until the caller resolves the attempt with `commit`/`COMMIT` or requests whole-transaction rollback. `transaction_failed()` is false for an unresolved commit attempt and true for retained failed-transaction rollback; check `pending_transaction_completion()` before resuming work. SQL statements, SQL cursors and the direct document, retrieval and graph queries described below admit their SERIALIZABLE participant at the first data snapshot through the common storage session. The [implementation plan](../../plans/0008-concurrent-storage-transactions.md) tracks remaining transaction and provider acceptance.

Repeating COMMIT after an unresolved commit resolves the same evaluated storage batch and does not rerun the Rust callback, deferred triggers, held-cursor materialization or temporary-table COMMIT actions. If rollback discovers a matching committed receipt, Engine finishes that commit's session publication and returns `25000` explaining that rollback could not undo it. If a later COMMIT confirms a recorded abort, Engine restores the rolled-back session state and returns `25000` instead of a successful COMMIT.

If statement-error cleanup cannot finish storage rollback, Engine retains the transaction frame, selected caches and logical locks for later cleanup. An uncertain rollback retains its original completion identity and returns `08007`; a failure before any completion attempt keeps the transaction failed and blocks ordinary work with `25P02`. COMMIT on that failed transaction still requests rollback and cannot publish its private writes. COMMIT AND CHAIN or ROLLBACK AND CHAIN starts the next transaction only after the retained rollback finishes.

The same uncertain-outcome contract applies when a transaction callback returns an error or panics, deferred commit validation fails, or an implicit document/graph operation cannot finish rollback. Cleanup context preserves `08007`, including through storage error wrappers and `close()`. A scoped callback that reports an uncertain rollback leaves the original attempt for explicit completion resolution; dropping the scope does not silently retry it, even if the provider has become available again.

An unresolved result must not be treated as proof of rollback or a reason to replay application operations. Session receipt resolution does not provide crash-safe publication recovery or exactly-once acknowledgement after process loss.

The development transaction adapter preserves typed storage diagnostics: a rejected MVCC row or definition conflict reports `40001`, cancellation reports `57014` as `SQLError::Cancelled`, and memory exhaustion reports `53200`. Embedded SQL diagnostics, including constraint errors, retain their SQLSTATE; an unrelated provider error is not classified as a serialization failure. An indeterminate outer commit remains `08007` even when its underlying diagnostic describes a conflict. A rejected commit restores the session after storage rollback, preserves independently committed data, and does not replay application callbacks. These diagnostics do not enable the unfinished concurrent SQL transaction model.

An error or panic while applying a direct mutation or document read inside an explicit transaction uses the same abort boundary as SQL: private data, index changes and logical write intents roll back to the active user savepoint or transaction frame. `transaction_failed()` remains true until recovery. `ROLLBACK TO SAVEPOINT` restores the usable savepoint while preserving earlier writes; COMMIT of an unrecovered failed frame performs rollback. The original error or panic is preserved when cleanup succeeds, and a cleanup failure reports both causes. Existing read dependencies remain retained according to the isolation contract.

`get_document(table, doc_id)`, `table_doc_ids(table)` and `document_count(table)` use the active transaction's selected data view and retain an AccessShare relation lock until transaction completion. A persistent session without an explicit transaction opens and completes one implicit read transaction using its default isolation, read-only and deferrable settings. A direct read inside a SQL callback shares that statement's snapshot. SERIALIZABLE observes absent document IDs and empty scans/counts as well as returned data; concurrent changes can therefore cause commit to report `40001`. An unknown table reports `42P01`, a read in a failed transaction reports `25P02`, and cancellation of deferrable admission reports `57014`. For example, after `begin()`, calling `get_document("items", id)` and then `commit()` keeps the read and its retained lock in that transaction. Internal execution adapters retain their caller's existing scope and keep query reads separate from current-row mutation checks. `document_count` counts documents retained by the table's text index; a row without indexed text fields contributes no indexed document.

`has_table`, `table_columns`, `table_has_column`, `table_names`, `describe_table` and their `try_` aliases use the same first-query transaction boundary before metadata lookup. A first metadata query establishes the data snapshot used by subsequent REPEATABLE READ or SERIALIZABLE queries, including after savepoint undo. Implicit queries use the persistent session's defaults and complete before returning; nested callbacks retain their enclosing statement's view. Failed or unresolved transactions and cancelled admission retain their typed diagnostics through `StorageBackendError`. Metadata lookup preserves existing catalog visibility and does not manufacture a user-row read dependency. Internal execution and restoration adapters use their already selected scope.

Schema and namespace lookup/enumeration, current-schema resolution, index metadata, sequence listing/state snapshots, view lookup/enumeration, named and field analyzer lookup, `analyze_text`, foreign server/table lookup/enumeration, table constraint/default-expression queries, `load_model` and `deep_predict_features` enter this same boundary. Their `try_` aliases have the same transaction behavior. `SQLError` and `StorageBackendError` results retain typed transaction diagnostics; APIs returning `String` return the transaction diagnostic as text. Lookup or validation failure aborts the active transaction frame or savepoint, and unresolved completion rejects the query before catalog lookup. Backend-owned fixed readers keep their existing snapshot and completion owner.

`find_doc_id_by_field(table, field, value)` and `find_conflict(table, columns, values)` use the same transaction boundary, selected table view and retained AccessShare lock as document reads. Field lookup requires a stored field equal to the supplied value; conflict lookup treats a missing field as NULL and returns `None` for an empty or mismatched key. Those early results still honor transaction admission, failed-frame rejection and pending completion. Conflict lookup selects the integer primary-key slot, the first answerable column index or an evaluated scan. SERIALIZABLE records the chosen row or index key, observes candidate rows used to check additional fields, and observes the relation for scans, including absent results. Private row changes mask their original documents and supply replacement matches. Internal mutation checks retain their current document/index view and reuse the enclosing statement.

`column_stats`, `try_column_stats`, `fts_index_stats` and standalone `optimised_tree_for` also enter the selected transaction before lookup or planning, including cached statistics and predicates that produce no operator tree. Column-statistics refresh retains the existing `ANALYZE` maintenance, read-only and rollback behavior. Full-text statistics read the selected table/index data and retain its relation identity and lock; enumeration on an attached reader uses that reader's table inventory. Internal SQL statistics and planner workers reuse their enclosing statement without entering another public query boundary.

Public text, profiled text, KNN, vector similarity, model-calibrated vector and hybrid search use this same boundary before planning or accessing analyzers and indexes. Their table binding retains an identity-checked AccessShare lock. `bayesian_params_for`, `calibration_report`, `deep_predict` and public `EngineDriver::execute_node` also enter the selected transaction before reading their inputs. Queries that may persist automatic calibration own a writable rollback snapshot when necessary, including on memory engines. Read-only transactions estimate missing or stale automatic calibration from their selected corpus without reserving a parameter writer or publishing parameters; explicit parameter saves and learning still report `25006`. A later writable query estimates and persists parameters normally. A later query error rolls back any calibration publication, and parameter names retain the caller's original table spelling. Graph reads, graph/label/path-index lookup and listing, and `run_cypher` use the same session defaults and first-snapshot boundary. Errors returned by these operations abort an active frame or savepoint; owned implicit frames finish before results are returned. Nested calls retain their enclosing statement's view, and physical worker adapters use that existing scope.

`Engine::sql_batch` executes a slice of SQL statement and parameter pairs in one transaction. A statement failure requests rollback; an indeterminate completion follows the resolution contract above. `Engine::sql_batch_with_labels` has the same transaction contract and returns [enum labels](#enum-labels-in-results).

```rust
use uqa_core::Value;
use uqa_engine::SQLParam;

let first = [
    SQLParam::scalar(Value::Int(1)),
    SQLParam::scalar(Value::Int(100)),
];
let second = [
    SQLParam::scalar(Value::Int(2)),
    SQLParam::scalar(Value::Int(50)),
];
let statements = [
    ("INSERT INTO accounts (id, balance) VALUES ($1, $2)",
     &first[..]),
    ("INSERT INTO accounts (id, balance) VALUES ($1, $2)",
     &second[..]),
];
engine.sql_batch(&statements)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Do not issue concurrent statements through the same session while an explicit transaction is active. Create independent sessions instead.

## Sessions

`Engine::new_session()` creates a new SQL session over the same persistent provider. Each session has independent transaction state, session variables, prepared statements, statement caches, and cancellation tokens. Durable rows, catalog objects, indexes, graph data, and runtime UDF registries are shared.

`Engine::new_session_for_user(user)` opens an independent session for a role authenticated by the embedding host. It verifies that the role exists, has `LOGIN`, and has database `CONNECT`, then sets both `session_user` and `current_user` to that role. The host owns credential verification and connection limits. The [PostgreSQL TCP server](11-postgresql-server.md) uses this entry point after its configured authentication policy accepts a connection.

Session creation requires a persistent backend that can return an independent, transaction-affine catalog/data pair. Engine retains an explicit provider or adapts the backend's session factory when constructed through `from_persistent_backends`.

When the parent has a stable committed catalog and no private transaction or temporary namespace, a new session shares immutable schema, constraint, and durable-registry allocations instead of reloading and decoding them. Each session retains its own mutable catalog owners and physical storage handles; a mutation detaches the affected catalog value. A parent with private state, a changed storage generation, or a provider without a usable commit monitor requires a load from the new session's committed storage view. Statement catalog snapshots also share these immutable values.

Persistent engines automatically maintain statistics in a separate database-level worker session; applications need not issue ANALYZE. The first collection runs automatically, subsequent refreshes become eligible after `50 + analyzed_row_count / 10` committed changes or 60 seconds with pending changes, and the worker checks approximately once per second. Updates are sampled from at most 4,096 hierarchy rows, project scalar columns, and do not hydrate opaque media/vector/JSON payloads. Existing statistics remain available until replacement. Pending changes survive reopen and follow transaction/savepoint rollback. `Engine::automatic_statistics_status()` reports worker activity, completed refreshes, and the last failure; failures retain pending work for retry. `Engine::column_stats(table)` returns current cached statistics, including sampled scalar statistics, and refreshes dirty statistics through the same transaction boundary as `ANALYZE`, including its read-only and rollback behavior. Explicit `ANALYZE` or `Engine::run_analyze` forces full collection regardless of cached statistics. Collected column statistics remain private until the transaction commits and revert on transaction or savepoint rollback, including in a read-only SQL transaction. Clean statistics are reused after reopen, and persistent SQL query planning never invokes this synchronous maintenance API. Memory-only engines retain automatic lazy collection without a persistent worker.

Persistent DiskANN indexes use the database maintenance worker to prune covered or committed-obsolete changes and rebuild their immutable graph. Each census or pruning page examines at most 64 keys from a fixed discovery snapshot. A rebuild consumes the exact canonical/catalog capture used for its census; later commits stay in the change path and older readers retain their original views. The worker uses independent provider transactions, the original memory/cancellation controls and its shared encrypted-temporary allowance. Successful pending steps continue without waiting a second between pages; idle and failed work retain the approximate one-second polling interval. One admitted build can span multiple physical staging batches and remains cancellable within Storage.

`DiskANNRebuildPolicy::default()` triggers reconstruction at 1,024 current uncovered documents or 64 MiB of logical raw vector bytes. Empty replacements count as documents; raw bytes are not compressed-file usage, journal size or reclaimed MVCC history. `DiskANNRebuildPolicy::new(documents, vector_bytes)` requires two positive thresholds. `Engine::diskann_rebuild_policy()` reads the policy and `Engine::set_diskann_rebuild_policy(policy)` changes it for this process's sessions sharing the database maintenance coordinator. The setting is not persisted. Admitted work keeps its original policy; a change also revisits pending changes on an otherwise unchanged database. These soft triggers do not increase memory or temporary limits or impose a write quota.

`Engine::automatic_diskann_maintenance_status()` returns `DiskANNMaintenanceStatus`. `completed_passes`, `examined` and `removed` retain their journal-pruning meanings. `completed_censuses` and `last_census` report complete captured change counts and their original generation; that generation is not the newly built head. `completed_rebuilds` advances only after confirmed publication. `phase` identifies counting, pruning, rebuilding or completion; `pending_completion` and `last_error` retain unresolved outcome/failure visibility. Commit retry resolves the original evaluated build without reconstructing it. The final Engine client releases the worker and its retained resources. See [automatic reconstruction](../../design/diskann-maintenance-capture.md#automatic-reconstruction) and the [journal contract](../../design/diskann-journal-pruning.md#automatic-finite-passes).

## Receive SQL notifications

Execute `LISTEN`, `UNLISTEN`, `NOTIFY`, and `pg_notify` through `Engine::sql`. `Engine::poll_sql_notifications()` imports committed messages without draining them, `Engine::wait_for_sql_notifications(timeout)` waits for availability, and `Engine::take_sql_notifications()` drains them as `SQLNotification { process_id, channel, payload }`, where `process_id` matches the sending session's `Engine::backend_process_id()` and SQL `pg_backend_pid()`. Sessions made with `new_session()` and independently opened engines share one bounded database-scoped queue while retaining independent subscriptions, cursors, and drained messages; on native file-backed databases the same queue also coordinates independent OS processes.

```rust
let directory = tempfile::tempdir()?;
let root = uqa_engine::Engine::open(&directory.path().join("notifications.db"))?;
let listener = root.new_session()?;
let sender = root.new_session()?;
listener.sql("LISTEN jobs", &[])?;
sender.sql("NOTIFY jobs, 'ready'", &[])?;
assert!(listener.wait_for_sql_notifications(std::time::Duration::from_secs(1))?);
let messages = listener.take_sql_notifications();
assert_eq!(messages[0].channel, "jobs");
assert_eq!(messages[0].payload, "ready");
assert_eq!(messages[0].process_id, sender.backend_process_id());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Subscription changes and outgoing messages take effect at outer commit, rollback and savepoint rollback discard their transactional changes, and identical channel-and-payload pairs are delivered once per transaction. Polling, waiting, or draining while the listener has an open transaction returns no messages and leaves them queued until the transaction ends. SQL exposes committed channels through `pg_listening_channels()` and queue occupancy through `pg_notification_queue_usage()`. Native file-backed coordination uses an explicitly versioned sidecar for the queue, database-wide backend-process identifier allocation, and listener-liveness leases; opaque identities and targets without native process support remain process-local. An embedding server must drain messages and encode the existing `uqa-pg-wire::NotificationResponse` itself because the engine does not own its client connection.

`open_encrypted`, encrypted `open_auto`, and compressed-encrypted constructors protect notification payloads and channels in the sidecar with the same credential as the main database. SQLite provider and backend factories preserve that protection for independently opened engines and new sessions. An existing plaintext sidecar or a mismatched sidecar key causes open to fail; it is never silently overwritten or opened without encryption. Custom encrypted file providers must implement `auxiliary_encryption_key` on their provider and backend. See [auxiliary storage encryption](../internals/03-storage.md#encryption-and-compression) for ownership and upgrade boundaries.

### Notification policy for stateless SQL hosts

UQA Engine 0.4.7 provides `Engine::require_notification_subscriptions() -> Result<(), SQLError>`. A host serving SQL without a retained notification session must call it before executing requests. It rejects SQL `LISTEN` and `UNLISTEN` with `SQLError::NotificationRequiresSubscription`: `code()` returns `Some("NOTIFICATION_REQUIRES_SUBSCRIPTION")` and `sqlstate()` returns `Some("0A000")`. `NOTIFY`, `pg_notify` and independently owned subscription handles remain available.

Configuration requires an idle session with no SQL transaction or committed SQL listener; otherwise it returns SQLSTATE `55000`. Successful configuration is irreversible for that session and repeated calls are harmless. Rollback, `DISCARD`, nested execution and cached/prepared plans retain the restriction. Subsequently created sibling sessions inherit it; existing peers retain their own policy. Ordinary Engine sessions keep their PostgreSQL notification behavior by default.

Direct `LISTEN`/`UNLISTEN` in one SQL message or anywhere in `sql_batch` are rejected before that batch executes or emits results. Commands reached through routines, triggers or dynamic SQL are rejected at execution and retain normal rollback and exception-handler behavior. Comments, strings, notification payloads and unevaluated function definitions are not treated as commands. The [preservation argument](../../design/stateless-notification-policy.md) states these boundaries. The HTTP server adapter and wire error mapping remain tracked separately in the [implementation ledger](../../plans/0015-sql-notifications-and-sse.md).

```rust
let session = uqa_engine::Engine::new();
session.require_notification_subscriptions()?;
let error = session.sql("LISTEN jobs", &[]).unwrap_err();
assert_eq!(error.code(), Some("NOTIFICATION_REQUIRES_SUBSCRIPTION"));
session.sql("NOTIFY jobs, 'ready'", &[])?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Independent owned listeners

UQA Engine 0.4.7 provides `Engine::subscribe_notifications(&[&str], NotificationSubscriptionOptions) -> Result<NotificationSubscription, NotificationSubscriptionError>`. The returned handle is ready for all requested channels and remains independent of the caller's transaction and low-level SQL listener. It uses the original memory database or retained persistent provider, including encryption, and retains the effective role incarnation selected at registration. It neither creates a new SQL session nor reconnects through HTTP. Channels must be unique, nonempty, NUL-free exact UTF-8 strings of at most 63 bytes; invalid input fails before registering any channel.

Every option is required and positive: `max_active_subscriptions`, `max_channels`, `max_queued_notifications`, `max_queued_bytes` and `max_registry_entries_per_poll`. Pending registrations and retained handles share the active-subscription allowance on the same hub and must satisfy every retained permit's ceiling. Admission rejects capacity or concurrent admission-metadata contention before entering another wait; a failed registration releases its permit, while a ready handle retains it until cleanup releases its resources. Queue bytes include retained string and queue capacity, private prepared deliveries and buffer replacement overlap. Original provider caches, fixed listener/channel/admission state and values already returned to the application remain separate resources. These options have no deployment defaults; the small example below supplies limits for its two-message fixture.

`subscribe_notifications_with_cancellation(channels, options, &CancellationToken)` additionally accepts an independent registration signal. Cancellation interrupts Engine gate waits and native registry lock/statement/commit waits and returns the stable `Cancelled` failure category without partially registering channels. Successful registration may win a simultaneous cancellation race. After readiness this token does not close the handle; use its explicit close/drop lifecycle. The caller's SQL cancellation and transaction remain independent. Operating-system file opening or provider construction already in progress remains synchronous, so this API does not promise an end-to-end registration or cleanup deadline.

Runtime adapters reserve admission before submitting work with `reserve_notification_subscription(options, &CancellationToken) -> Result<NotificationSubscriptionPermit, NotificationSubscriptionError>`. This operation uses the original shared allowance without waiting on Engine or provider gates. The opaque permit fixes all five options and is consumed once by `subscribe_notifications_with_permit(channels, permit, &CancellationToken)`, which performs the same native registration without acquiring another slot. A different hub rejects the permit with `InvalidRequest`; the permit is not an authorization grant. Retain the original Engine separately while work is pending. Dropping an unused permit releases only capacity metadata and performs no provider I/O; successful registration transfers the same permit through the listener's completed cleanup. An adapter must reserve this capacity before placing registration in its runtime queue; this Rust interface does not establish a particular language adapter.

`poll()` returns `Poll::Pending`, `Poll::Ready(Some(NotificationEvent))` or `Poll::Ready(None)` after closure without a retained failure. `wait(Duration)` returns `NotificationWait::Event`, `TimedOut` or `Closed`; a receive timeout does not unsubscribe. `next_event().await` returns `Some(NotificationEvent)` or `None` on closure without blocking a thread or requiring a particular async runtime. All three preserve a retained terminal error, including after cleanup. Serialize consumption of a handle; a second pending asynchronous receive returns `InvalidRequest`. Dropping a pending receive releases its single wake slot without consuming an event or unsubscribing. Embedded events use a fresh `NotificationIdentity.epoch`, no HTTP request ID and contiguous exact `u64` sequences beginning at one. The original notification retains its channel, payload and sender process ID.

`close()` is idempotent and wakes blocked receivers; concurrent close calls wait for the same retained-resource cleanup, and dropping the handle closes it. `is_closed()` reports delivery closure, which can precede completed provider cleanup. `stop_delivery()` supplies only the non-I/O wake/closure signal for runtime adapters; registration and admission remain retained until the adapter calls and joins `close()` outside its event-loop worker. The last external owner cancels and joins shared recovery before releasing its provider; other Engine or subscription owners keep recovery alive. Close all retained handles before replacing a database file. Registration and close can perform native I/O; asynchronous runtime adapters must schedule that work outside their event-loop worker. The [cleanup argument](../../design/owned-notification-listeners.md#cleanup-completion-and-recovery-shutdown) states the completion boundary and remaining native-I/O limits.

An independent handle's close releases its local listener and native liveness lease without acquiring the shared notification-registry writer. Other processes observe retirement through their existing lease checks. Registration, publication, queue-usage and recovery operations reap the obsolete registry metadata before using live-listener cursors. Closing one handle preserves other listeners and their shared recovery task; the last retained owner joins that task before returning.

Count or byte overflow terminates the affected receiver with `NotificationFailureKind::Backpressure`. Source failures and sequence exhaustion also remain visible to every subsequent poll; another receiver cannot consume that error. `NotificationSubscriptionError::kind()` and `code()` expose closed content-free diagnostics, while `original_error()` explicitly inspects any private local cause. A terminal error does not imply replay. The application must create another subscription explicitly after handling loss of continuity.

```rust
use std::time::Duration;
use uqa_core::notifications::NotificationEvent;
use uqa_engine::{Engine, NotificationSubscriptionOptions, NotificationWait};

let engine = Engine::new();
let subscription = engine.subscribe_notifications(&["jobs", "results"], NotificationSubscriptionOptions {
    max_active_subscriptions: 1,
    max_channels: 2,
    max_queued_notifications: 2,
    max_queued_bytes: 4_096,
    max_registry_entries_per_poll: 2,
})?;
engine.sql("BEGIN; NOTIFY jobs, 'ready'; NOTIFY results, 'done'; COMMIT", &[])?;
for (expected_sequence, expected_channel) in [(1, "jobs"), (2, "results")] {
    let NotificationWait::Event(NotificationEvent::Notification { sequence, notification, .. }) = subscription.wait(Duration::from_secs(1))? else {
        panic!("expected committed notification");
    };
    assert_eq!(sequence, expected_sequence);
    assert_eq!(notification.channel, expected_channel);
}
subscription.close();
# Ok::<(), Box<dyn std::error::Error>>(())
```

The [ownership and preservation argument](../../design/owned-notification-listeners.md) describes cursor publication, queue/admission limits, controlled registration and final-owner cleanup. The [implementation ledger](../../plans/0015-sql-notifications-and-sse.md) tracks remaining startup/cleanup bounds, complete resource qualification and HTTP/language adapters; this Rust API does not imply those interfaces are available.

## Document and retrieval APIs

The API also exposes typed operations that bypass SQL text:

- Table and field setup: `create_default_table`, `create_vector_field`
- Documents: `add_document`, `add_document_with_vectors`, `get_document`, `delete_document`, `document_count`
- Vectors: `add_vector`, `knn_search`, `vector_similarity_search`
- Text: `search`, `search_profiled`
- Hybrid: `hybrid_search` for exact single-prior log-odds fusion and `robust_hybrid_search` for explicitly requested positive-evidence pooling
- Calibration: Bayesian parameter fitting and calibration reports

SQL and typed operations use the same durable state and indexes. Applications can mix them, but a single transaction boundary should use one clear ownership path.

Direct document insertion, replacement, field updates and deletion register changed persistent row identities with the active SERIALIZABLE transaction, independently of text, value or vector index changes. Field updates, patches and deletion observe their selected persistent row before fetching it, including an absent row. An absent target registers a point read without registering a row write, so it participates in serialization conflicts even when no document changes. Statement and savepoint rollback discard write intents together with the private changes; read observations and dependencies already established with other transactions remain. Whole-transaction rollback releases that transaction's read observations too.

## Fixed-model DiskANN calibration

`Engine::diskann_calibration_target(table, field, embedding_model_id, embedding_model_version, candidate_k)` returns a `uqa_scoring::VectorCalibrationTarget` for the actual selected DiskANN index. Table and field are direct-API names; the two embedding identifiers must be nonempty and remain the application's identity contract. `candidate_k` must be positive. The target includes resolved table/field names, dimensions, index kind, requested document count and opaque corpus/index versions verified by Storage.

Use that target when fitting a reusable model, then pass the same model and target to `calibrated_vector_search_with_model`. Execution retains one index snapshot, checks its actual canonical view and physical generation, and searches that same snapshot. Changed vector data, private writes, recreation, rebuilds and changed search settings invalidate an incompatible target. Unrelated table or model-catalog writes do not change the vector version. Rollback and retained readers use their selected earlier versions. Unknown metadata and mismatches are errors; they never trigger model refitting or select SQL's query-pool estimator.

The following assumes `docs.embedding` already has a two-dimensional DiskANN index. Its transform is a numerical fixture; applications supply independently fitted parameters and the corresponding fit sample count.

```rust
use uqa_scoring::{VectorCalibrationModel, VectorCalibrationProvenance, VectorProbabilityTransform};

let target = engine.diskann_calibration_target("docs", "embedding", "fixture", "1", 3)?;
let model = VectorCalibrationModel::new(
    VectorProbabilityTransform::new(0.0, 1.0, 1.0, 0.5)?,
    VectorCalibrationProvenance {
        model_version: "fixed-fixture".into(),
        target: target.clone(),
        fit_sample_count: 100,
    },
)?;
let rows = engine.calibrated_vector_search_with_model(
    "docs", "embedding", [1.0, 0.0], &model, &target,
)?;
```

The target lookup enters the ordinary direct-table read transaction and retains its relation lock, but does not execute KNN or register a serializable vector predicate. The actual model search retains normal vector read observations. Private target versions belong to the original view/process, and restored storage history can invalidate a previous target. Other index methods retain their existing caller-controlled version contract. Saving a model does not automatically apply it to `calibrated_vector_match` or hybrid SQL. See the [identity and preservation argument](../../design/diskann-calibration-identity.md).

## Analyzer APIs

Persistent custom analyzers are managed with `register_named_analyzer`, `list_named_analyzers`, `set_table_field_analyzer`, `table_field_analyzer`, `get_table_analyzer`, and `drop_named_analyzer`. Compatibility aliases use the shorter create, set, and drop names. An index-time or both-phase assignment rebuilds current postings; a search-only assignment changes query analysis without a rebuild.

The `uqa-analysis` crate exposes `Analyzer`, `CharFilter`, `Tokenizer`, and `TokenFilter` for constructing and previewing pipelines directly. Its process-global registry is not a replacement for engine catalog persistence. See [Text analyzer pipelines](06-text-analyzers.md) for JSON tags, component behavior, phase resolution, SQL examples, synonyms, and failure rules.

## Graph APIs

Named graph methods cover graph creation, deletion, listing, vertex and edge mutation, Cypher execution, traversal, and path indexes. SQL can address the same graph state through `cypher`, `rpq`, and `graph_*` functions. See [Graphs](07-graphs.md).

## Runtime extensions

Register scalar, table, and aggregate functions with:

- `register_scalar_function` and `register_scalar_function_with_options`
- `register_table_function` and `register_table_function_with_options`
- `register_aggregate_function` and `register_aggregate_function_with_options`

Default options are conservative: a function is `VOLATILE` and may mutate. Use `SQLFunctionOptions::read_only(volatility)` only when the callback is truly read-only. A callback that mutates state must remain `VOLATILE`.

Runtime callbacks are not serialized to persistent storage. Register them each time the process constructs an engine. New sessions share the runtime registry of their parent engine.

## Cancellation

Each session has its own cancellation token. `cancel()` requests cancellation, `is_cancelled()` observes it, and the reset API clears the request before later work. Long-running execution paths poll the token at safe boundaries.

Cancellation is cooperative. Handle the returned error, inspect `transaction_failed()` and recover an aborted explicit transaction through rollback or an available savepoint before continuing. Reset the session cancellation token before issuing subsequent work.

## QueryBuilder

The `uqa-api` crate provides `QueryBuilder` for fluent construction of select, filter, retrieval, graph, aggregate, facet, fusion, and model expressions. Use it when programmatic composition is clearer than assembling SQL text. Always bind user data or use builder methods that quote values; do not interpolate untrusted strings into raw fragments.

See [Bindings and extensions](08-bindings-and-extensions.md) for a broader API matrix.
