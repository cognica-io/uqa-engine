# PostgreSQL regular-expression functions

Status: Active

The regular-expression differences filed together as #980 through #983 are delivered one unit per pull request in this order: the `substring` pattern forms (#981), set-returning `regexp_matches` (#982), the split functions (#983), and finally the shared engine semantics (#980). The first unit also exposed SQL-syntax printing of `POSITION`, `OVERLAY`, `TRIM` and `SUBSTRING` (#984), the missing Unicode normalization functions (#985) and further CONTEXT evidence for #306; those remain separate issues.

## Units

| Issue | Unit | State |
| --- | --- | --- |
| [#981](https://github.com/cognica-io/uqa-engine/issues/981) | `substring` pattern forms | Implemented with focused verification; automatic Linux/macOS acceptance pending |
| [#982](https://github.com/cognica-io/uqa-engine/issues/982) | `regexp_matches` as a set-returning function | Open |
| [#983](https://github.com/cognica-io/uqa-engine/issues/983) | `regexp_split_to_table` through the SQL owner and `regexp_split_to_array` | Open |
| [#980](https://github.com/cognica-io/uqa-engine/issues/980) | ARE match selection, constraints, collation-dependent classes and diagnostics | Open |

## Substring pattern forms (#981)

### Ownership and correction

SQL owns overload resolution, the regular-expression kernels and scalar execution. Overload binding already resolved `substring(text, text)` and `substring(text, text, text)`, but evaluation dispatched by name and converted the second and third arguments to integers. The binding now records `FunctionDispatch::RegexSubstring` or `FunctionDispatch::SimilarSubstring` from the resolved argument types, as `to_bin` and `random` record their selected overloads, and the dispatched PostgreSQL scalar kernels pass those calls to the regular-expression owner's `eval_dispatched_substring`. The POSIX form compiles the pattern through the existing regular-expression owner and returns the first parenthesized subexpression or the whole match. The SQL form applies the existing `similar_to_escape` kernel and then the same extraction, which is PostgreSQL's own definition `substring($1, similar_to_escape($2, $3))`. Engine, storage, dependencies, feature selections and persisted record formats are unchanged; stored plans persist the new dispatch variants as they persist earlier overload-selected variants.

### Preservation argument

Dispatch is a function of the resolved signature only. The positional signatures `(text, integer)`, `(text, integer, integer)` and their bytea counterparts map to no dispatch, so their calls reach the unchanged positional kernel exactly as before. Only `(text, text)` and `(text, text, text)` map to the new variants; before this change those calls failed with `42804` for every non-numeric pattern and returned a positional slice when the pattern text parsed as an integer. Every previously successful result of a positional signature is therefore preserved, and the only changed observations are those of the two pattern signatures, which now follow PostgreSQL's definitions.

For the POSIX form, PostgreSQL's `textregexsubstr(s, p)` returns the span of the first parenthesized subexpression `m1` when `p` has one and the whole match `m0` otherwise, and NULL when the selected span is absent. The implementation computes the same function of `(m0, m1)` from the owner's captures, so any remaining difference from PostgreSQL is confined to the engine's choice of `m0` and `m1`, which #980 tracks. For the SQL form, PostgreSQL defines `substring(s, p, e) = substring(s, similar_to_escape(p, e))`. The implementation composes the existing `similar_to_escape` kernel, whose translation text equals PostgreSQL's for every independently observed pattern, with the same POSIX extraction, so the same argument applies. Both forms are strict in PostgreSQL's catalog; the explicit NULL check and the strict dispatch flag preserve that.

Evaluation reads only its arguments and the per-call compiled pattern. It does not touch storage, catalog state, row support, multiplicity or ordering. The input copy, translated pattern and result are admitted under the caller's allowance; only the result survives a successful call, and a rejected output releases its reservation. Both cancellation scopes are checked before and after matching.

### Verification

The independent Docker PostgreSQL 18.4 transcript `crates/uqa-sql/src/expr/regex/tests/substring-postgresql.json` (47 statements, captured with `tests/parity/pg18/capture_command_completion_oracle.py` in a disposable database) passes on memory, native SQLite, SQLite Key/Value and redb, including a stored view read after reopening. Before the correction, 29 of its 34 single-statement queries differed. Three SQL owner tests replay its constant cases and check admission and cancellation, and the fixed-overload binding test covers all four `substring` signatures. Remaining differences inside these forms are match selection and the text of invalid-pattern messages (#980), CONTEXT for errors raised inside the SQL-language form (#306) and SQL-syntax printing in stored definitions (#984); the manifest item `functions.substring-pattern-forms` is therefore `partial`. Automatic Linux/macOS acceptance is required before delivery.
