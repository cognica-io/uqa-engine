# Bounded COPY output

Issue [#714](https://github.com/cognica-io/uqa-engine/issues/714) remains reproducible by source inspection at main `6b9f8cb87`: Execution obtains a complete `SQLResult`, SQL creates all encoded bytes in a `Vec`, and Execution then writes that buffer. The stream-shaped public API therefore retains two complete results. Completion requires actual large-output memory, byte correctness, snapshot, cancellation and failing-sink evidence; source inspection alone does not satisfy the issue.

## Delivery units

- [x] SQL encoding: expose one incremental text/CSV encoder, admit each formatted field through the current allowance, write escaped byte spans directly to the sink, and preserve exact existing bytes and I/O diagnostics. Reuse this implementation from the existing materializing codec APIs so format semantics have one owner.
- [ ] Execution output: remove the complete query result using the existing bounded query-output interfaces. Preserve ordinary relation and query semantics, duplicate output labels, zero-column rows, DML RETURNING and data-modifying CTEs. A SELECT-only cursor cannot silently replace the current general statement path.
- [x] Engine adapter: retain the correct session snapshot, statement context, cancellation and resource ownership through output. Keep algorithms and tests in SQL/Execution; Engine covers the public lifetime contract and storage adapters.
- [ ] Acceptance: a synthetic export larger than its allowance, exact Unicode/NULL/empty/quote/bytes/vector output, short writes, interruption, partial sink failure, cancellation, snapshot stability and full cleanup; local checks and actual automatic Linux/macOS performance checks for delivered source.

The initial PR covers incremental encoding as one independently reviewable correction. The issue stays open until complete-query materialization and the remaining acceptance are resolved. Binary COPY and unrelated COPY FROM work are not claimed by this correction. Original performance tasks #1063, #348, #714, #1108, #1117 and #653 remain tracked; PR #1133 handles regex retention independently while its final evidence runs.

## Ownership and invariants

`uqa-sql` already owns COPY option parsing, text/CSV escaping and scalar text conversion, and depends only on Core and the PostgreSQL wrappers. Its controlled scalar conversion interfaces admit textual output and preserve cancellation. `uqa-execution` owns statement scheduling and physical result delivery, and already depends on SQL; it supplies the output resource allowance and sink. Engine composes those interfaces with its session. No feature or dependency direction changes are required.

Encoding visits fields and rows in their original positional order, retaining at most one admitted formatted field and no encoded result buffer. It preserves all escaping, header and NULL decisions and stops at the first error. Partial writes and interrupted writes must check current cancellation before retrying. Output failure must not be reported as successful completion or trigger a second execution of SQL, routines or triggers. Bounded query delivery must retain PostgreSQL transaction/error behavior and existing UQA result semantics.

## Incremental encoding verification

Local SQL verification passes sixteen selected cases, including eleven encoder cases. An 8MiB export retains at most 16KiB of admitted formatting scratch and releases it after each row; a large borrowed Unicode field allocates no owned text scratch. Every sink write is at most 8192 bytes. Tests cover exact hashes and byte counts, short and interrupted writes, both cancellation owners, I/O failure, zero writes and allocation refusal. Bytea conversion now reserves its known final length before writing, avoiding geometric reallocation and its transient double allowance.

Eight public Engine COPY cases pass, including DML RETURNING, modifying CTEs, duplicate output labels, session IntervalStyle, explicit-transaction abort after sink failure/cancellation, inheritance, partitions and durable reopening. Independent Docker PostgreSQL 18.4 output confirms text/CSV NULL, empty strings, Unicode, quoting, bytea, boolean, padded char, catalog vectors and custom CSV options. It also exposed the old single-column CSV end-marker ambiguity: a lone `\.` must be quoted, while the same field in a multi-column row need not be quoted; the shared encoder now preserves that PostgreSQL distinction.

Strict SQL/Execution/Engine library-and-test Clippy passes. Dependency, integration-harness, formatting and regression-inventory checks pass. Automatic Linux/macOS performance tests remain pending. No complete-query memory bound, new timing result or full #714 closure is claimed by this unit.

Final review also verifies an IntervalStyle change made by `set_config` inside the COPY query itself. Encoding reads the live session style for interval-bearing values, as ordinary typed output already does, and restores its caller scope. Both the SQL owner and the public API match independent PostgreSQL output after this change.

## Output failure and transaction publication

While preparing bounded query production, a public regression on integrated source `e3619d6ff` reproduced a separate correctness failure: an implicit `COPY (INSERT ... RETURNING ...) TO STDOUT` commits before encoding, so sink failure returns an error but leaves the inserted row in memory, native SQLite, SQLite K/V and redb, including durable reopen. Docker PostgreSQL 18.4 rolls back a mutating COPY whose output to `/dev/full` fails. Issue [#1141](https://github.com/cognica-io/uqa-engine/issues/1141) tracks this concrete failure; the different I/O error codes are not compared as equivalent diagnostics.

The correction passes COPY result consumption through Execution's ordinary statement scheduler before its implicit commit. Encoding, flush and cancellation failures therefore enter the existing rollback/explicit-abort branches. SQL owns flushing the encoder sink, including interruption and invoking cancellation; Engine only forwards the session capability. Ordinary SQL and simple-query completion consumers keep their existing completion order. Output still observes the session's live formatting and work-memory settings after the query executes. The statement is never re-executed, and already emitted external bytes cannot be retracted.

The first transaction correction retained the complete query result. The next change below removes that vector for SELECT; command RETURNING and modifying CTE production and the original #714 acceptance remain open. Local validation passes 55 selected SQL/Execution/Engine cases, including all four providers, exact encoding, scratch bounds, cancellation, sink completion and existing simple-query completion behavior. The deterministic inventory includes the flush and four-provider rollback cases. Final automatic platform evidence must be checked before merge.

## Bounded SELECT delivery

Execution now hands the ordinary query executor's `SharedSpill` directly to the incremental SQL encoder inside the same statement boundary. The completed spool retains the original positional values and internal schema, including duplicate public labels, while the owning reader decodes one batch at a time. Query evaluation still precedes all external output, preserving volatile evaluation order and the completed source snapshot. Command output retains its ordinary path; command RETURNING and data-modifying CTE intermediate vectors require a separate producer correction before #714 can close.

Local verification passes both new Execution owner cases and all forty selected Engine COPY/cursor cases after correcting the memory-only fixture to respect its single-session API. The final four-provider snapshot selection passes 4/4. Owner cases exercise a 32MiB spilled result with exact SHA-256, byte and row counts, a fixed 512KiB allocation ceiling, one successful flush, partial sink failure, pre-delivery cancellation and removal of temporary files. The public four-provider case exercises a 2048-row query under 1KiB work memory, duplicate labels, a concurrent update from the output sink for the three persistent providers, cached set queries and empty CSV headers. The deterministic inventory includes these cases; actual platform results remain required before merge.

The first transaction-publication commit also passes all 508 selected local transaction, savepoint and TPC-H cases. Those broader results belong to `46c43c8d9`; the final SELECT-spool change uses the focused owner/public checks above. The inventory now contains 1,187 checks / 1,693 required cases. Actual automatic Linux/macOS evidence for the final source remains pending; no timing claim is made.

Final SQL/Execution/Engine library-and-test Clippy with denied warnings passes for the SELECT-spool change, as do formatting, dependency/ownership and harness checks. The unrelated session-index fixture failure in integrated-source performance CI is tracked in #1143 and is being corrected without weakening its assertions.

## Upstream schema integration

The COPY publication branch incorporates the completed lexical-lineage and correlated-schema changes from `6ebc2289b`, including the regression fixture dependency-reuse correction. The only overlapping paths were HISTORY and the regression inventory; every independent source file remains byte-identical to its originating unit. The three-way inventory merge preserves both sets of assertions (1,191 checks / 1,697 required cases), and all 33 regression-runner tests pass with process inspection enabled. Automatic combined-source results remain required before merge.
