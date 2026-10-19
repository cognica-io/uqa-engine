# Bounded COPY output

Issue [#714](https://github.com/cognica-io/uqa-engine/issues/714) remains reproducible by source inspection at main `6b9f8cb87`: Execution obtains a complete `SQLResult`, SQL creates all encoded bytes in a `Vec`, and Execution then writes that buffer. The stream-shaped public API therefore retains two complete results. Completion requires actual large-output memory, byte correctness, snapshot, cancellation and failing-sink evidence; source inspection alone does not satisfy the issue.

## Delivery units

- [x] SQL encoding: expose one incremental text/CSV encoder, admit each formatted field through the current allowance, write escaped byte spans directly to the sink, and preserve exact existing bytes and I/O diagnostics. Reuse this implementation from the existing materializing codec APIs so format semantics have one owner.
- [ ] Execution output: remove the complete query result using the existing bounded query-output interfaces. Preserve ordinary relation and query semantics, duplicate output labels, zero-column rows, DML RETURNING and data-modifying CTEs. A SELECT-only cursor cannot silently replace the current general statement path.
- [ ] Engine adapter: retain the correct session snapshot, statement context, cancellation and resource ownership through output. Keep algorithms and tests in SQL/Execution; Engine covers the public lifetime contract and storage adapters.
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
