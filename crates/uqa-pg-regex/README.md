# uqa-pg-regex

`uqa-pg-regex` compiles PostgreSQL 18.4's Advanced Regular Expression engine, Henry Spencer's library from PostgreSQL's `src/backend/regex`, from unchanged and checksummed PostgreSQL sources, and exposes it to UQA Engine through a small safe Rust API.

## API

- `Regex::compile(pattern, flags, collation, resources)` runs `pg_regcomp` with PostgreSQL's `REG_*` compile flags (`CompileFlags`) under a `RegexCollation`.
- `Regex::exec(input, search_start, matches)` runs `pg_regexec` on code points from a search start and fills `MatchSpan` slots, slot 0 for the whole match and slot `n` for subexpression `n`; `Regex::subexpression_count` is `re_nsub`.
- `RegexCollation::from_oid` and `RegexCollation::from_locale` apply `pg_set_regex_collation`'s rules and errors to UQA's predefined collations and to collations described by their catalog properties.
- `RegexError` carries the engine's `REG_*` code with `pg_regerror`'s exact explanation, a cancellation, or an error PostgreSQL raises with its own SQLSTATE, message and hint.
- `RegexResources` admits every engine allocation, chunk headers included, before it happens and receives every byte back after it is freed. The same hooks answer the engine's interrupt and stack-depth checks. Dropping a `Regex` deletes its memory context, as `RE_compile_and_cache` does, and releases every reserved byte.
- The engine recurses on the stack. Every compilation and execution may use `MAX_STACK_DEPTH`, PostgreSQL's default `max_stack_depth` of 2 MB, of engine stack and fails with `REG_ETOOBIG` beyond it; the stack-depth hook may set a lower limit. Each operation runs with that depth and PostgreSQL's `STACK_DEPTH_SLOP` of 512 KiB available: on the thread's own stack when it has that much, and otherwise on a separately allocated stack of the same thread through the `stacker` crate. An operation on a thread that lacks the headroom first runs where it is and moves to a separate stack only when it outgrows the thread's stack, so ordinary operations on threads with a few megabytes of stack allocate nothing. Under Emscripten the engine's recursion uses the WebAssembly call stack, which can be neither measured nor switched. There the engine counts its live frames exactly and charges each a fixed depth, calibrated in Node.js so that `MAX_STACK_DEPTH` corresponds to half of V8's default stack at the largest engine frame measured, 1493 frames; every compilation and execution fails with `REG_ETOOBIG` before it can exhaust the call stack, whatever its pattern and input. Typical compilations and matches run about 12% slower for the counting, and native builds are not instrumented.

The engine is compiled with `-O2` in every build profile, as PostgreSQL compiles it, because the stack each recursion level uses depends on the optimization level. It also depends on the compiler and architecture, as it does for PostgreSQL itself: a level of nested capturing groups takes about 240 bytes with GCC on x86-64, 320 with GCC on AArch64 and 400 with Clang on AArch64, so the deepest pattern accepted under the 2 MB budget differs between platforms.

Character classes, case-insensitive matching and word boundaries follow PostgreSQL's strategy for the collation: the ASCII table for `C`, PostgreSQL's own Unicode 16 tables for the builtin provider, and ICU's character functions, evaluated over ICU4X Unicode 16 data, for ICU collations. Engine state is kept per operation and per thread, so operations on different threads are independent.

## Verification

The crate's tests compare compilation, match selection, subexpression spans, diagnostics and collation selection with an independent PostgreSQL 18.4 server, recorded in `tests/fixtures/regex_oracle.expected.json`, including the members of every character class over all code points under the C, builtin and ICU strategies and patterns that PostgreSQL accepts below and rejects beyond its default stack depth. Fault-injection tests refuse each allocation and cancel at each interrupt check of representative compilations and executions, and stack tests run operations that need several times the stack their thread has left.

## Updating the import

The imported files, their checksums and every local adaptation are listed in [UPSTREAM.md](UPSTREAM.md). Do not edit imported files. Review the upstream changes of every listed file, update the pinned tag and commit in `scripts/sync-uqa-pg-regex.py`, and run:

```sh
python3 scripts/sync-uqa-pg-regex.py
python3 scripts/sync-uqa-pg-regex.py --check
```

## License

The imported PostgreSQL sources keep the PostgreSQL License and, for the regular expression package, Henry Spencer's license and the Tcl license; UQA's wrapper and shims use the PostgreSQL License. See [LICENSE](LICENSE).
