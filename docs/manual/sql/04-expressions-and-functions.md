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
| Formatting | `to_hex`, `to_number` |

`to_number(text, 'RN')` reads the PostgreSQL Roman-numeral prefix after leading whitespace, accepts values from 1 through 3999, and ignores input after that prefix. Random state is session-local. Use `setseed` for deterministic test input, not for cryptographic randomness; `gen_random_uuid` and `uuidv4` produce random version 4 UUIDs, while `uuidv7([shift interval])` produces time-ordered version 7 UUIDs.

## Array functions

| Functions | Purpose |
| --- | --- |
| `array_length`, `array_lower`, `array_upper`, `cardinality` | Dimensions and bounds |
| `array_cat`, `array_append`, `array_prepend` | Construction |
| `array_remove`, `array_replace`, `array_trim`, `array_sample`, `array_sort`, `array_reverse` | Transformation |
| `array_position`, `array_positions`, `array_overlap` | Search and overlap |
| `array_to_string`, `array_fill` | Conversion and construction |
| `unnest` | Expand values as a table function |

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
