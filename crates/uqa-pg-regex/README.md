# uqa-pg-regex

`uqa-pg-regex` compiles PostgreSQL 18.4's Advanced Regular Expression engine, Henry Spencer's library from PostgreSQL's `src/backend/regex`, from unchanged and checksummed PostgreSQL sources, and exposes it to UQA Engine through a small safe Rust API.

## API

- `Regex::compile(pattern, flags, collation, resources)` runs `pg_regcomp` with PostgreSQL's `REG_*` compile flags (`CompileFlags`) under a `RegexCollation`.
- `Regex::exec(input, search_start, matches)` runs `pg_regexec` on code points from a search start and fills `MatchSpan` slots, slot 0 for the whole match and slot `n` for subexpression `n`; `Regex::subexpression_count` is `re_nsub`.
- `RegexCollation::from_oid` and `RegexCollation::from_locale` apply `pg_set_regex_collation`'s rules and errors to UQA's predefined collations and to collations described by their catalog properties.
- `RegexError` carries the engine's `REG_*` code with `pg_regerror`'s exact explanation, a cancellation, or an error PostgreSQL raises with its own SQLSTATE, message and hint.
- `RegexResources` admits every engine allocation, chunk headers included, before it happens and receives every byte back after it is freed. The same hooks answer the engine's interrupt and stack-depth checks. Dropping a `Regex` releases every reserved byte.

Character classes, case-insensitive matching and word boundaries follow PostgreSQL's strategy for the collation: the ASCII table for `C`, PostgreSQL's own Unicode 16 tables for the builtin provider, and ICU's character functions, evaluated over ICU4X Unicode 16 data, for ICU collations. Engine state is kept per operation and per thread, so operations on different threads are independent.

## Verification

The crate's tests compare compilation, match selection, subexpression spans, diagnostics and collation selection with an independent PostgreSQL 18.4 server, recorded in `tests/fixtures/regex_oracle.expected.json`, including the members of every character class over all code points under the C, builtin and ICU strategies. Fault-injection tests refuse each allocation and cancel at each interrupt check of representative compilations and executions.

## Updating the import

The imported files, their checksums and every local adaptation are listed in [UPSTREAM.md](UPSTREAM.md). Do not edit imported files. Review the upstream changes of every listed file, update the pinned tag and commit in `scripts/sync-uqa-pg-regex.py`, and run:

```sh
python3 scripts/sync-uqa-pg-regex.py
python3 scripts/sync-uqa-pg-regex.py --check
```

## License

The imported PostgreSQL sources keep the PostgreSQL License and, for the regular expression package, Henry Spencer's license and the Tcl license; UQA's wrapper and shims use the PostgreSQL License. See [LICENSE](LICENSE).
