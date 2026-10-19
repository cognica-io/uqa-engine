# Bounded COPY output

Issue [#714](https://github.com/cognica-io/uqa-engine/issues/714) remains reproducible by source inspection at main `6b9f8cb87`: Execution obtains a complete `SQLResult`, SQL creates all encoded bytes in a `Vec`, and Execution then writes that buffer. The stream-shaped public API therefore retains two complete results. Completion requires actual large-output memory, byte correctness, snapshot, cancellation and failing-sink evidence; source inspection alone does not satisfy the issue.

## Delivery units

- [ ] SQL encoding: expose one incremental text/CSV encoder, admit each formatted field through the current allowance, write escaped byte spans directly to the sink, and preserve exact existing bytes and I/O diagnostics. Reuse this implementation from the existing materializing codec APIs so format semantics have one owner.
- [ ] Execution output: remove the complete query result using the existing bounded query-output interfaces. Preserve ordinary relation and query semantics, duplicate output labels, zero-column rows, DML RETURNING and data-modifying CTEs. A SELECT-only cursor cannot silently replace the current general statement path.
- [ ] Engine adapter: retain the correct session snapshot, statement context, cancellation and resource ownership through output. Keep algorithms and tests in SQL/Execution; Engine covers the public lifetime contract and storage adapters.
- [ ] Acceptance: a synthetic export larger than its allowance, exact Unicode/NULL/empty/quote/bytes/vector output, short writes, interruption, partial sink failure, cancellation, snapshot stability and full cleanup; local checks and actual automatic Linux/macOS performance checks for delivered source.

The initial PR covers incremental encoding as one independently reviewable correction. The issue stays open until complete-query materialization and the remaining acceptance are resolved. Binary COPY and unrelated COPY FROM work are not claimed by this correction. Original performance tasks #1063, #348, #714, #1108, #1117 and #653 remain tracked; PR #1133 handles regex retention independently while its final evidence runs.

## Ownership and invariants

`uqa-sql` already owns COPY option parsing, text/CSV escaping and scalar text conversion, and depends only on Core and the PostgreSQL wrappers. Its controlled scalar conversion interfaces admit textual output and preserve cancellation. `uqa-execution` owns statement scheduling and physical result delivery, and already depends on SQL; it supplies the output resource allowance and sink. Engine composes those interfaces with its session. No feature or dependency direction changes are required.

Encoding visits fields and rows in their original positional order, retaining at most one admitted formatted field and no encoded result buffer. It preserves all escaping, header and NULL decisions and stops at the first error. Partial writes and interrupted writes must check current cancellation before retrying. Output failure must not be reported as successful completion or trigger a second execution of SQL, routines or triggers. Bounded query delivery must retain PostgreSQL transaction/error behavior and existing UQA result semantics.
