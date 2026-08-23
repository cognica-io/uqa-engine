# Expressions and Functions

This chapter is a name-level catalog of built-in expression functions. Most signatures follow familiar PostgreSQL forms, but only the names and argument shapes implemented by UQA Engine are available. Validate a migration with executable fixtures instead of assuming the complete PostgreSQL overload set.

## Operators and conditional expressions

Implemented expression families include arithmetic, comparison, Boolean logic, NULL tests, `BETWEEN`, `IN`, `EXISTS`, `LIKE`, `ILIKE`, regular-expression matching, `SIMILAR TO`, concatenation, array construction and subscripting, casts, and searched or simple `CASE`.

```sql
SELECT CASE
           WHEN score >= 0.8 THEN 'high'
           WHEN score >= 0.5 THEN 'medium'
           ELSE 'low'
       END AS band
FROM predictions;
```

`LIKE`, `ILIKE`, and `SIMILAR TO` accept `ESCAPE` with a runtime text expression. Omitting the clause uses PostgreSQL's default backslash escape, `ESCAPE ''` disables escaping, `ESCAPE NULL` produces NULL, and every nonempty escape must contain exactly one character. Escaped wildcard and regular-expression metacharacters are treated literally, while escaped alphanumeric characters in `SIMILAR TO` retain the implemented PostgreSQL regular-expression escape behavior.

```sql execute
SELECT value
FROM (VALUES ('a_b'), ('axb')) AS candidates(value)
WHERE value LIKE 'a!_b' ESCAPE '!';
```

## NULL and comparison helpers

| Functions | Purpose |
| --- | --- |
| `coalesce`, `nullif` | NULL selection and conditional NULL |
| `greatest`, `least` | Extremum across scalar arguments |
| `num_nulls`, `num_nonnulls` | Count NULL or non-NULL arguments |

## Text functions

| Group | Functions |
| --- | --- |
| Case and shape | `upper`, `lower`, `casefold`, `initcap`, `reverse` |
| Length | `length`, `char_length`, `character_length`, `octet_length`, `bit_length` |
| Trim and pad | `trim`, `btrim`, `ltrim`, `rtrim`, `lpad`, `rpad` |
| Composition | `concat`, `concat_ws`, `replace`, `repeat`, `translate`, `overlay`, `format` |
| Slicing and location | `substring`, `substr`, `left`, `right`, `position`, `strpos`, `starts_with`, `split_part` |
| Pattern and regular expression | `like`, `ilike`, `similar_to`, `regexp_match`, `regexp_matches`, `regexp_replace`, `regexp_count`, `regexp_instr`, `regexp_like`, `regexp_substr` |
| Quoting | `quote_ident`, `quote_literal`, `quote_nullable` |
| Character conversion | `ascii`, `chr` |
| Arrays and tables | `string_to_array`, `array_to_string`, `string_to_table`, `regexp_split_to_table` |
| Hash and encoding | `md5`, `crc32`, `crc32c`, `encode`, `decode` |

`casefold` uses the Unicode 16 full default case-fold mapping. The regular-expression functions accept PostgreSQL 18 named argument notation; `regexp_replace` also implements its `start` and `N` overloads.

`reverse(text)` reverses Unicode scalar values and `reverse(bytea)` reverses raw bytes. An unknown literal, NULL, or untyped parameter selects the preferred `text` overload; `varchar`, `character`, `name`, and internal `"char"` inputs are implicitly converted to `text`, while unrelated types and every call other than one positional argument report PostgreSQL's undefined-function SQLSTATE `42883`. Both overloads are strict, immutable, parallel-safe, and available through `pg_catalog`; unqualified user overloads participate in PostgreSQL search-path, exact-match, and preferred-type resolution before a stable function binding is stored in generated expressions.

`md5(text)` hashes the text value's UTF-8 bytes and `md5(bytea)` hashes the raw byte payload; both return a 32-character lowercase hexadecimal `text` digest without changing database state. An unknown literal, NULL, or untyped parameter selects the preferred `text` overload; character-family values are implicitly converted to `text`, while unrelated types, named arguments, and every arity other than one report SQLSTATE `42883`. Both overloads are strict, immutable, parallel-safe, leakproof, available through `pg_catalog`, and bound with the same PostgreSQL search-path and exact-match rules used by generated expressions.

```sql execute
SELECT md5('abc') AS text_hash,
       md5(decode('00ff10', 'hex')) AS bytea_hash;
```

`crc32(bytea)` and `crc32c(bytea)` compute PostgreSQL's CRC-32 and CRC-32C checksums over the raw byte payload and return nonnegative `bigint` values in the unsigned 32-bit range. Because each function has only a `bytea` overload, an unknown literal, NULL, or untyped parameter binds as `bytea`; explicit text, character, numeric, array, named-argument, and non-one-argument calls report SQLSTATE `42883`. User-defined overloads participate in PostgreSQL's string-category, preferred-type, and search-path ranking, and an unresolved unknown call reports SQLSTATE `42725` instead of silently selecting the built-in. Both functions are strict, immutable, parallel-safe, leakproof, available through `pg_catalog` as OIDs 6364 and 6365, and generated expressions retain the selected binding across reopen.

```sql execute
SELECT crc32(decode('00ff10', 'hex')) AS crc32,
       crc32c(decode('00ff10', 'hex')) AS crc32c;
```

The one-argument length family preserves PostgreSQL's declared string and binary overloads. `length(text)`, `char_length(text)`, and `character_length(text)` count Unicode characters; their `character` overloads ignore trailing blank padding. `length(bytea)` counts raw bytes. `octet_length(text)` and `octet_length(bytea)` count UTF-8 or raw payload bytes, while `octet_length(character)` includes declared blank padding. `bit_length(text)` and `bit_length(bytea)` return eight times the corresponding byte count; a `character` input reaches the text overload and therefore discards trailing padding. Every overload returns `integer` and is strict, immutable, parallel-safe, and not leakproof.

An unknown literal, NULL, or untyped parameter selects the preferred `text` overload. `varchar`, `name`, and internal `"char"` values convert to `text`; unrelated types, named arguments, and non-one-argument calls report SQLSTATE `42883` when no separate PostgreSQL overload exists. Exact built-in and user-defined overloads follow PostgreSQL search-path precedence, and generated expressions retain the selected binding across reopen. This documented slice does not describe PostgreSQL's separate two-argument `length(bytea, name)` encoding function or length overloads for types outside the implemented carriers.

```sql execute
SELECT length('é') AS characters,
       octet_length('é') AS utf8_octets,
       octet_length('a'::char(3)) AS padded_octets,
       length(decode('00ff10', 'hex')) AS raw_octets,
       bit_length(decode('00ff10', 'hex')) AS raw_bits;
```

## Numeric functions

| Group | Functions |
| --- | --- |
| Basic | `abs`, `sign`, `round`, `trunc`, `ceil`, `ceiling`, `floor` |
| Powers and roots | `power`, `pow`, `sqrt`, `cbrt`, `gamma`, `lgamma`, `exp`, `ln`, `log`, `log10`, `log2` |
| Division and number theory | `mod`, `div`, `gcd`, `lcm`, `factorial` |
| Trigonometric | `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2` |
| Hyperbolic | `sinh`, `cosh`, `tanh` |
| Angles and constants | `pi`, `degrees`, `radians` |
| Bucketing | `width_bucket` |
| Random | `random`, `setseed` |
| Formatting | `to_bin`, `to_oct`, `to_hex`, `to_number` |

`to_bin`, `to_oct`, and `to_hex` accept PostgreSQL's exact `integer` and `bigint` overloads and return lowercase, unprefixed text; negative values use the argument type's 32-bit or 64-bit two's-complement representation. `to_number(text, 'RN')` reads the PostgreSQL Roman-numeral prefix after leading whitespace, accepts values from 1 through 3999, and ignores input after that prefix.

`random()` returns a `double precision` value from 0.0 inclusive to 1.0 exclusive. `random(min, max)` has exact `integer`, `bigint`, and `numeric` overloads and samples both bounds inclusively; mixed integer arguments select PostgreSQL's promoted overload, and a numeric result uses the greater fractional scale of its bounds. NULL bounds produce NULL, a lower bound greater than the upper bound and non-finite numeric bounds report SQLSTATE `22023`, and equal bounds do not advance the random stream. Random state is session-local and nontransactional, so failed statements and transaction or savepoint rollback leave consumed draws and `setseed` changes in place; `setseed` reproduces PostgreSQL's sequence across the unit and range forms. Use these functions for deterministic tests and non-cryptographic sampling only; `gen_random_uuid` and `uuidv4` produce random version 4 UUIDs, while `uuidv7([shift interval])` produces time-ordered version 7 UUIDs.

## UUID functions

| Function | Result |
| --- | --- |
| `gen_random_uuid()`, `uuidv4()` | Random RFC variant version 4 UUID |
| `uuidv7([shift interval])` | Time-ordered RFC variant version 7 UUID |
| `uuid_extract_version(uuid)` | RFC 9562 version nibble as `smallint`, or NULL for a non-RFC variant including the nil UUID |
| `uuid_extract_timestamp(uuid)` | Version 1 or version 7 timestamp as `timestamp with time zone`, or NULL for every other version or variant |

The extraction functions are strict and immutable. Version 1 timestamps use the UUID 100-nanosecond epoch and are floored to PostgreSQL's microsecond precision, while version 7 timestamps use the leading 48-bit Unix millisecond field; sub-millisecond random or counter bits do not affect the extracted timestamp.

## Array functions

| Functions | Purpose |
| --- | --- |
| `array_length`, `array_lower`, `array_upper`, `cardinality` | Dimensions and bounds |
| `array_cat`, `array_append`, `array_prepend` | Construction |
| `array_remove`, `array_replace`, `array_trim`, `array_sample`, `array_sort`, `array_reverse` | Transformation |
| `array_position`, `array_positions`, `array_overlap` | Search and overlap |
| `array_to_string`, `array_fill` | Conversion and construction |
| `unnest` | Expand values as a table function |

`array_reverse(anyarray)` reverses the first dimension, and `array_sort(anyarray [, descending boolean [, nulls_first boolean]])` orders first-dimension elements while preserving dimensions and lower bounds. The result retains its concrete base-array type, including PostgreSQL's flattening of an array domain to that base type. The two- and three-argument sort overloads accept PostgreSQL's `"array"`, `descending`, and `nulls_first` named notation in declaration-independent order; unknown string literals and bare parameters in Boolean slots receive Boolean context, explicit non-Boolean arguments are rejected, NULL arguments are strict, and an unknown array argument cannot determine the polymorphic type. Unqualified calls participate in normal overload resolution: an exact concrete user-function overload can outrank the polymorphic built-in, an implicit-only user candidate conflicts with a viable built-in, and `pg_catalog` qualification selects the built-in directly. Sorting uses PostgreSQL element, nested-array, and record ordering for the implemented types, including the same `json` comparison-function errors, while reversing does not require an element comparator.

## JSON and JSONB functions

| Group | Functions |
| --- | --- |
| Construction | `json_build_object`, `jsonb_build_object`, `json_build_array`, `jsonb_build_array`, `to_json`, `to_jsonb`, `row_to_json` |
| Type and size | `json_typeof`, `jsonb_typeof`, `json_array_length`, `jsonb_array_length` |
| Extraction | `json_extract_path`, `jsonb_extract_path`, `json_extract_path_text`, `jsonb_extract_path_text` |
| Containment and keys | `json_contains`, `json_contained_by`, `json_has_key`, `json_has_any_key`, `json_has_all_keys` |
| SQL/JSON path | `jsonb_path_exists`, `jsonpath_exists`, `jsonb_path_match`, `jsonpath_match` |
| Mutation | `jsonb_set`, `jsonb_insert`, `json_delete_path` |
| Formatting | `jsonb_pretty`, `json_strip_nulls`, `jsonb_strip_nulls` |
| Expansion | `json_each`, `jsonb_each`, `json_each_text`, `jsonb_each_text`, `json_array_elements`, `jsonb_array_elements`, `json_array_elements_text`, `jsonb_array_elements_text`, `json_object_keys`, `jsonb_object_keys` |

JSON expansion functions are table functions when used in `FROM`.

`json_strip_nulls` and `jsonb_strip_nulls` accept PostgreSQL 18's optional `strip_in_arrays` Boolean argument. The default removes object fields whose value is JSON null while retaining null array elements; `true` removes both.

## Temporal functions

| Group | Functions |
| --- | --- |
| Current time | `now`, `current_timestamp`, `current_date`, `clock_timestamp`, `statement_timestamp`, `timeofday` |
| Conversion | `to_timestamp`, `to_date`, `to_char` |
| Parts and truncation | `extract`, `date_part`, `date_trunc` |
| Arithmetic and construction | `age`, `make_timestamp`, `make_date`, `make_interval`, `justify_hours` |
| Validation | `isfinite` |

## Session and identity functions

Implemented helpers include `current_database`, `current_catalog`, `current_user`, `session_user`, `current_schema`, `current_schemas`, `typeof`, and `pg_typeof`. `current_schema` and `current_schemas` follow the session `search_path`.

## Spatial helpers

`point`, `st_distance`, `st_within`, `st_dwithin`, and `overlaps` provide the implemented point and range operations. UQA Engine does not expose an SQL R-tree index access method, so verify physical behavior for spatial workloads.

## Sequence functions

`nextval`, `currval`, and `setval` operate on named sequences. `currval` requires the sequence to have produced or received a value in the relevant session context.

## Aggregate functions

| Group | Functions |
| --- | --- |
| Count and numeric | `count`, `sum`, `avg`, `min`, `max` |
| Text and arrays | `string_agg`, `array_agg` |
| Boolean | `bool_and`, `bool_or` |
| Statistics | `stddev`, `stddev_samp`, `stddev_pop`, `variance`, `var_samp`, `var_pop` |
| Ordered set | `percentile_cont`, `percentile_disc`, `mode` |
| JSON | `json_agg`, `jsonb_agg`, `json_object_agg`, `jsonb_object_agg` |

Aggregates support `DISTINCT`, aggregate-local `ORDER BY`, and `FILTER` where the function shape permits it. `min` and `max` compare arrays and record-like map values lexicographically in addition to their scalar inputs.

```sql
SELECT department,
       count(*) FILTER (WHERE active) AS active_count,
       string_agg(name, ', ' ORDER BY name) AS names
FROM employees
GROUP BY department;
```

Ordered-set examples use `WITHIN GROUP`:

```sql
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY latency_ms) AS median
FROM samples;
```

## Window functions

Ranking and offset windows are `row_number`, `rank`, `dense_rank`, `lag`, `lead`, and `ntile`. Aggregate windows are `sum`, `count`, `avg`, `min`, and `max`.

## General table functions

| Function | Implemented shape |
| --- | --- |
| `generate_series(start, stop [, step])` | Integer series with two or three arguments |
| `unnest(array)` | One row per array value |
| `regexp_split_to_table(text, pattern)` | One row per split value |
| `string_to_table(text, delimiter)` | One row per split value |
| JSON expansion functions | Key/value or element rows |
| Registered table callbacks | Schema returned by the callback |

Table functions can be aliased with a relation name and column definition list. `WITH ORDINALITY` and multi-function `ROWS FROM` are not implemented.

## Analyzer and index table functions

`create_analyzer`, `drop_analyzer`, `list_analyzers`, `set_table_analyzer`, and `fts_index_stats` manage or inspect full-text analyzers and indexes. Mutating analyzer functions participate in transaction state. The full JSON schema, phase semantics, diagnostics columns, persistence, and errors are documented in [Analyzer SQL](05-analyzers.md).

```sql
SELECT * FROM create_analyzer(
    'strict',
    '{"tokenizer":{"type":"keyword"}}'
);

SELECT * FROM set_table_analyzer(
    'documents', 'body', 'strict', 'both'
);

SELECT field, analyzer, indexed_doc_count, term_count
FROM fts_index_stats('documents');
```

## Retrieval and graph functions

Retrieval and graph names have plan-level semantics rather than ordinary row-by-row scalar semantics. They are documented separately in [Retrieval SQL](06-retrieval.md) and [Graph SQL and Cypher](07-graph.md).
