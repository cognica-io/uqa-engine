# SQL notification subscriptions and SSE implementation

Status: active. The [design](../design/sql-notifications-and-sse.md) is the full acceptance contract. Work starts from Engine `b82d8d146dd6bda7bfef7d1642b0ae285b0a3dc1`; no subscription API or HTTP endpoint is qualified yet.

## Ownership and delivery boundaries

Engine retains the original database, selected SQL authorization, notification hub and listener lifetime. Core owns transport-independent values. Storage and its providers own durable publication, recovery and bounded registry reads. Execution owns statement dispatch, including the stateless-session restriction; Engine supplies the session policy. The HTTP client owns framing, transport and reconnect state and cannot depend on Engine. Each language adapter owns its actual runtime cancellation and iterator behavior.

The existing authenticated HTTP server is `crates/cloud-node` in the sibling [UQA Cloud repository](https://github.com/cognica-io/uqa-cloud), not this workspace's fluent `uqa-api` query builder. Its current dependency pin is Engine `0.3.8` at `a714e63f9d0e9ebcb61f217a640a7fe4b801548e`; server integration requires an explicit source-pin update and compatibility qualification after the required Engine interfaces are merged. Server changes, actual HTTP acceptance and serving-path budgets are required work, not optional follow-ups. Implementing a mock route in client tests cannot close the server requirement. This implementation does not request a deployment or manual package publication.

## Fixed completion ledger

There are ten completion units. A unit may need more than one focused review, but review boundaries do not remove requirements or change the denominator. Keep at most one active implementation PR per repository, complete review and merge before creating the next dependent PR, and commit and push coherent changes as they become reviewable. Each row remains incomplete until its exit evidence exists at the relevant source revision.

| Unit | Owner and scope | Required exit evidence | State |
| --- | --- | --- | --- |
| Shared subscription values | Core value, epoch, event and failure categories; compatible Engine re-export | Exact legacy value identity, bounded canonical identities, exact sequence values, redacted event diagnostics and representation-preservation proof | Merged in PR #231 at `d4fd5ca7` |
| Retained direct listener | Engine lifecycle and authorization; bounded delivery at the hub; provider-owned bounded reads | Atomic all-channel readiness, independent registration boundaries/cursors, count and byte admission, visible overflow, cancellation, selected role, original memory/persistent/encrypted provider and lease lifetime | In progress; borrowed provider reads and native cancellation implemented |
| Stateless SQL restriction | Execution dispatch and SQL admission; Engine session policy | Typed `NOTIFICATION_REQUIRES_SUBSCRIPTION` on direct, batch and nested `LISTEN`/`UNLISTEN`; early batch validation, rollback and unchanged `NOTIFY`/`pg_notify` | Pending |
| SSE protocol | Client request, decoder and wire validation | Strict bounded request and every event schema, every byte split, UTF-8/CRLF/multiline data, duplicate/unknown field rejection, canonical identities/decimals, sequence checks, bounded frame/line/nesting and long cumulative streams; valid depth-two requests accepted, depth-three containers rejected before recursive materialization | Pending |
| Rust HTTP subscription | Client owned transport and lifecycle | Readiness before return, original-origin security, five distinct budgets, explicit gap/reconnection events, one attempt at a time, bounded retries and close/drop cancellation | Pending |
| Authenticated server | Cloud node listener adapter, auth identity, resource admission and incremental HTTP writes | Real Engine publication, pre-stream error codes, retained and revalidated authority, bounded registration/queues/writes, flushed heartbeats, draining, privacy and documented complete resource/timing profile | Pending |
| Python subscriptions | Embedded and HTTP bindings | Actual wheel; sync context/iterator with GIL release, async context/iterator, exact integers, stable typed failures and cancellation of owned work | Pending |
| Node.js subscriptions | Native embedded adapter and existing HTTP-only JavaScript client | Actual addon and addon-free package; Promise registration, bigint, async iterator, AbortSignal/return/close and nonblocking waits | Pending |
| Browser subscriptions | Existing WASM runtime bridge and authenticated fetch client | Actual browser artifact; direct runtime identity, no event-loop blocking, bigint, bearer/CORS/credentials policy and iterator/AbortSignal cleanup | Pending |
| Integrated qualification and public contract | All owners, supported providers and bindings | All fifteen design cases below, source-bound evidence, mathematical preservation argument, version-matched manual/examples and automatic CI/review closure | Pending |

## Acceptance matrix

The case numbers below refer to the unchanged list in design section 10. A passing unit test or mock HTTP response does not substitute for the actual owner, provider or language artifact named in the row.

| Design case | Required scenario | Responsible completion units |
| --- | --- | --- |
| 1 | Real Engine and HTTP commit, rollback and transaction duplicate suppression | Direct listener, server, integrated qualification |
| 2 | Registration race, effective boundary, no old buffered history and no message theft | Direct listener, server |
| 3 | Multiple channels, overlapping ordered subscribers and independent close | Direct listener, all bindings |
| 4 | Every framing split, malformed and oversized input, long cumulative stream; accept the valid depth-two request, reject depth-three containers, and ignore escaped string delimiters in depth counting | SSE protocol, all HTTP bindings |
| 5 | Revocation, idle expiry, permission changes, queued writes and fresh reconnect authority | Direct listener, server, Rust HTTP subscription |
| 6 | Accounted idle/burst/escaped-payload/slow-consumer memory; healthy subscriber and SQL isolation | Direct listener, server, integrated qualification |
| 7 | Loss, lost ready, restart, draining and visible gap before replacement data | Rust HTTP subscription, server, all HTTP bindings |
| 8 | Cancel connection, registration, receive, blocked write and reconnect; release actual owned resources | Direct listener, Rust HTTP subscription, server, all bindings |
| 9 | Idle streams outlive SQL deadlines; delayed heartbeat, silent loss and full budget arithmetic | Rust HTTP subscription, server, all HTTP bindings |
| 10 | Actual Rust/Python/Node/browser protocol, GIL, asyncio, iterator and CORS behavior | All bindings, integrated qualification |
| 11 | Real direct/batch/nested stateless restrictions and unchanged transactional publication | Stateless SQL restriction, server |
| 12 | Actual TRACE and error privacy with closed bounded metadata only | Server, Rust HTTP subscription, all bindings |
| 13 | Direct delivery with no HTTP, nonblocking runtime waits and real cancellation | Direct listener, all embedded bindings |
| 14 | Caller transaction/low-level queue isolation, independent handle close and file leases | Direct listener, all embedded bindings |
| 15 | Persistent/encrypted/memory identity and selected authorization; each provider's process and browser scope | Direct listener, all embedded bindings, integrated qualification |

## Invariants and proof obligations

The design's ordered-history projection is the mathematical contract. Implementations must establish the actual registration and closing boundaries, independent cursors, exact value encoding, and visible termination before any skipped suffix. No adapter republishes a notification or changes transaction-local duplicate suppression. A new epoch after transport loss cannot claim continuity or replay. Extend the proof alongside the implementation that establishes each precondition; finite tests exercise those obligations but do not replace the argument.

Resource acceptance includes the original hub and durable registry, listener/channel ownership, each queued value, parser and transport buffers, and admission waiters. A bounded queue placed after an unbounded `entries_from` or destructive session drain is insufficient. Defaults require evidence for idle listeners, bursts, maximum escaped payloads and slow consumers. Timing qualification requires independent host-control/noise evidence for the measured margin; deterministic functional/accounted-resource checks do not establish that margin or a performance claim. Do not invent deployment defaults, weaken limits, or rerun uncontrolled measurements until they pass.

Use the existing single integration harness per crate and reuse established build artifacts. Run focused owner tests, strict Clippy, formatting and repository ownership/dependency checks for the actual change. PostgreSQL-sensitive behavior requires independent PostgreSQL 18 evidence. JVM tools run in Docker. CI remains responsible for its own runs and release publication; no manual dispatch, rerun, cancellation or package publication is part of this work.

## Current evidence

Initial inspection confirms that the existing Engine hub is shared by persistent identity or provider identity and memory-only sibling session creation is unsupported. Legacy session receivers use a destructive, unbounded `VecDeque`; cross-process delivery currently materializes every registry entry from a sequence. The direct handle therefore needs its own retained listener and bounded delivery at the publication/registry boundary. These findings select the implementation owners; they do not establish a completed subscription.

The shared-value change moves the original `SQLNotification` definition into Core and retains the existing Engine re-export. It adds canonical UUIDv4 epochs, bounded optional request identities, exact `u64` event counters, explicit gap/reconnection variants and closed failure categories. The [representation proof](../design/sql-notifications-and-sse.md#shared-value-representation) states the identity relocation and metadata-erasure laws. Five Core notification tests pass in the existing Linux Docker target, including RFC 9562's independent UUIDv4 vector, malformed identities, full-width integer/text preservation, diagnostic redaction and a codec property test. Strict Clippy with warnings denied passes the Core, Engine and Client library and test targets, including compilation of existing Engine notification callers. These checks do not claim runtime execution of the existing Engine suite or qualification of a subscription transport.

PR [#231](https://github.com/cognica-io/uqa-engine/pull/231) merged as `d4fd5ca7` after explicit authorization for an administrator squash merge of the reviewed head `7eb1bb2a`. Both automatic PR Checks runs passed; the full automatic CodeRabbit review's one actionable finding was corrected and its thread resolved. Copilot did not review because its quota was exhausted; the later CodeRabbit status is not counted as a second full review. The authorization covered the required approving review and missing `pre-merge CI` status. No manual workflow was dispatched, and the merged remote branch was deleted.

The provider-owned finite visitor over borrowed notification rows merged in PR [#232](https://github.com/cognica-io/uqa-engine/pull/232) as `a65fc6197`; the existing materialized read now uses that same reader. Twenty SQLite notification tests pass in the reused Linux Docker target, including four new cursor-resume, declined-row, maximum-payload, cancellation/error-precedence and malformed-text checks. The four affected scan cases also pass after adding the final cancellation check before confirming exhaustion. Strict Storage/SQLite library and test Clippy passes with warnings denied. Automatic formatting checks and the full automatic CodeRabbit review passed. The [scan proof](../design/sql-notifications-and-sse.md#borrowed-registry-scan-preservation) distinguishes accepted prefix progress from inspection, conservative exhaustion and callback ownership. The independent Engine listener, actual queue/admission bounds, authorization retention and async lifecycle remain required; this provider interface alone does not close the direct-listener unit.

The native-cancellation provider change retains the original `StorageReadControl` through pool admission, SQLite busy/progress callbacks and cancellable registration/cursor commits. Authoritative publication completion remains independent of the original query cancellation. The [preservation argument](../design/notification-registry-cancellation.md) covers rollback, moved and nested transactions, typed errors and callback restoration. All twenty-eight SQLite notification tests pass in an isolated source tree over the borrowed-read change, reusing the existing Linux Docker target. This includes actual blocked writer/commit waits, native VM/write interruption, cross-thread ownership, nested-signal retention and successful lease reuse. All SQLite provider source and test files match the previously validated native-cancellation revision exactly; its strict Storage/SQLite/Engine library and test Clippy evidence remains applicable to that unchanged provider code. Formatting, ownership and dependency checks pass for the isolated change. No new timing or process-memory claim is made, and Engine wiring remains separate.
