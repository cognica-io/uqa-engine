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

## Function overload resolution

For the implemented fixed-signature built-ins documented below, ordinary expressions and generated expressions use one PostgreSQL 18-style candidate-selection contract. Unqualified calls combine visible SQL user functions with `pg_catalog` candidates according to `search_path`; qualified names, exact and implicit matches, preferred types, unknown-category selection, domain base types, named arguments, defaults, and stored bindings use the same resolver. This contract is limited to the listed implemented signatures and does not imply support for PostgreSQL's complete built-in, polymorphic, operator, cast, or `pg_proc` matrix.

The shared fixed-signature registry covers `casefold`, `reverse`, `md5`, `crc32`, `crc32c`, the documented one-argument length family, `gamma`, `lgamma`, `json_strip_nulls`, `jsonb_strip_nulls`, `to_bin`, `to_hex`, `to_oct`, `to_regproc`, `to_regprocedure`, `to_regclass`, `to_regnamespace`, `to_regrole`, `to_regtype`, the unit and range `random` functions, and the documented UUID generation and extraction functions. Polymorphic array transformations retain their specialized type-substitution path.

## View definition functions

```sql
SELECT pg_get_viewdef(view_oid);
SELECT pg_get_viewdef(view_oid, pretty);
SELECT pg_get_viewdef(view_oid, wrap_column);
SELECT pg_get_viewdef(view_name);
SELECT pg_get_viewdef(view_name, pretty);
```

`view_oid` is an `oid` value, including a relation name cast to `regclass`; `view_name` is a `text` value resolved through the current search path and schema `USAGE` privileges. `pretty` is a Boolean value that defaults to false. The integer `wrap_column` overload enables pretty printing and controls target-list wrapping; zero places targets on separate lines and a negative value permits an unlimited line width.

The result is `text` containing the reconstructed SELECT command and its terminating semicolon for a regular or materialized view. Reconstruction reads the stored query without executing it, preserves fixed public output names, and chooses schema qualification against the caller's search path. View and source renames, replacement, transactions, savepoints, temporary relations, and durable reopen are reflected in subsequent calls. `pg_views.definition` and `pg_matviews.definition` expose the same default definition. `information_schema.views.view_definition` exposes it only to an enabled owning role; another role with view privileges sees NULL in that column.

Every overload propagates NULL arguments. An unknown OID or the OID or name of an existing non-view relation returns NULL. A missing textual relation reports `42P01`, a missing explicitly named schema reports `3F000`, and denied schema access reports `42501`; invalid names and unmatched overloads follow PostgreSQL's name and function-resolution errors. OID lookup does not require SELECT on the view. The routines are stable and parallel restricted, with their five PostgreSQL 18 signatures exposed in `pg_proc`.

```sql execute
CREATE TABLE definition_source (id integer, label text);
CREATE VIEW definition_example (item_id, label) AS
SELECT id, label FROM definition_source WHERE id > 0;
SELECT pg_get_viewdef('definition_example'::regclass) AS definition;
SELECT pg_get_viewdef('definition_example'::regclass, true) AS pretty_definition;
```

## Index definition functions

```sql
SELECT pg_get_indexdef(index_oid);
SELECT pg_get_indexdef(index_oid, column_number, pretty);
```

Both overloads return text reconstructed from the stored index metadata. The one-argument form and a zero `column_number` return the complete CREATE INDEX command without a terminating semicolon. Positive column numbers are one-based and return the selected key or included column without its ordering options; negative or out-of-range numbers return an empty string. The full definition preserves uniqueness, the access method, key order, NULL placement, included columns, `NULLS NOT DISTINCT`, and the partial predicate. Pretty output uses visible relation names and fewer parentheses. Unknown index OIDs and NULL arguments return NULL. Both PostgreSQL signatures are stable, strict, and parallel safe and are exposed in `pg_proc`.

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

`casefold(text)` uses the Unicode 16 full default case-fold mapping and returns `text`. It is strict, immutable, parallel-safe, not leakproof, and available through `pg_catalog`; unrelated types, named arguments, and every arity other than one report SQLSTATE `42883`. The regular-expression functions accept PostgreSQL 18 named argument notation; `regexp_replace` also implements its `start` and `N` overloads.

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

`gamma(double precision)` evaluates the gamma function and `lgamma(double precision)` evaluates the natural logarithm of its absolute value. PostgreSQL's implicit numeric conversions let `smallint`, `integer`, `bigint`, `numeric`, and `real` inputs reach the `double precision` signature, while unknown inputs participate in the same category, preferred-type, exact-match, and search-path ranking as user-defined overloads. Both functions are strict, immutable, parallel-safe, not leakproof, available through `pg_catalog` as OIDs 6383 and 6384, and retain their selected binding in generated expressions across reopen. Native builds call the host C math library as PostgreSQL does, so platform-specific last-bit results follow that library; targets without a native C ABI use the portable Rust math implementation. `gamma` reports SQLSTATE `22003` at poles, overflow, and underflow and for negative infinity while preserving positive infinity and NaN; `lgamma` reports `22003` at poles while preserving either infinity as positive infinity and preserving NaN. Invalid unknown text reports the `double precision` input error SQLSTATE `22P02`, and unsupported explicit signatures report `42883`.

`to_bin`, `to_oct`, and `to_hex` accept PostgreSQL's exact `integer` and `bigint` overloads and return lowercase, unprefixed text; negative values use the argument type's 32-bit or 64-bit two's-complement representation. Because neither overload is preferred, an unknown, NULL, or `smallint` argument without an explicit target is ambiguous and reports SQLSTATE `42725`; unrelated types, named arguments, and unsupported arities report `42883`. `to_number(text, 'RN')` reads the PostgreSQL Roman-numeral prefix after leading whitespace, accepts values from 1 through 3999, and ignores input after that prefix.

`random()` returns a `double precision` value from 0.0 inclusive to 1.0 exclusive. `random(min, max)` has exact `integer`, `bigint`, and `numeric` overloads and samples both bounds inclusively; mixed integer arguments select PostgreSQL's promoted overload, and a numeric result uses the greater fractional scale of its bounds. NULL bounds produce NULL, a lower bound greater than the upper bound and non-finite numeric bounds report SQLSTATE `22023`, and equal bounds do not advance the random stream. Random state is session-local and nontransactional, so failed statements and transaction or savepoint rollback leave consumed draws and `setseed` changes in place; `setseed` reproduces PostgreSQL's sequence across the unit and range forms. Use these functions for deterministic tests and non-cryptographic sampling only; `gen_random_uuid` and `uuidv4` produce random version 4 UUIDs, while `uuidv7([shift interval])` produces time-ordered version 7 UUIDs.

## UUID functions

| Function | Result |
| --- | --- |
| `gen_random_uuid()`, `uuidv4()` | Random RFC variant version 4 UUID |
| `uuidv7([shift interval])` | Time-ordered RFC variant version 7 UUID |
| `uuid_extract_version(uuid)` | RFC 9562 version nibble as `smallint`, or NULL for a non-RFC variant including the nil UUID |
| `uuid_extract_timestamp(uuid)` | Version 1 or version 7 timestamp as `timestamp with time zone`, or NULL for every other version or variant |

The extraction functions are strict and immutable. Version 1 timestamps use the UUID 100-nanosecond epoch and are floored to PostgreSQL's microsecond precision, while version 7 timestamps use the leading 48-bit Unix millisecond field; sub-millisecond random or counter bits do not affect the extracted timestamp.

`gen_random_uuid()` and `uuidv4()` accept no arguments. `uuidv7()` also accepts one `interval` argument whose declared name is `shift`; unsupported argument types, names, and arities report SQLSTATE `42883`. The three generator functions are volatile and therefore unavailable in generated expressions. This signature contract does not assert byte-for-byte UUID output or PostgreSQL's complete interval-shift edge semantics.

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

`json_strip_nulls(target json [, strip_in_arrays boolean DEFAULT false]) -> json` and `jsonb_strip_nulls(target jsonb [, strip_in_arrays boolean DEFAULT false]) -> jsonb` recursively remove object fields whose value is JSON null. The optional flag retains null array elements when omitted or `false` and removes them when `true`; `target` and `strip_in_arrays` support named notation in declaration-independent order. Both functions are strict, immutable, and parallel safe, do not change database state, preserve the input base return type, and accept domains over their declared argument types. Textual `json` results compact insignificant whitespace while preserving object order, duplicate keys, and numeric lexemes and decoding string escapes; `jsonb` results use normal binary-JSON key and numeric canonicalization. Calls with explicit `text`, the other JSON storage type, a non-Boolean flag, an unknown argument name, or an unsupported arity fail during overload resolution, while malformed unknown JSON input reports invalid JSON syntax.

```sql execute
SELECT json_strip_nulls(strip_in_arrays => true, target => '{"keep":1,"drop":null,"items":[null,{"drop":null}]}'::json);
```

## Temporal functions

| Group | Functions |
| --- | --- |
| Current time | `now`, `current_timestamp`, `current_date`, `clock_timestamp`, `statement_timestamp`, `timeofday` |
| Conversion | `to_timestamp`, `to_date`, `to_char` |
| Parts and truncation | `extract`, `date_part`, `date_trunc` |
| Arithmetic and construction | `age`, `make_timestamp`, `make_date`, `make_interval`, `justify_hours` |
| Validation | `isfinite` |

## Range and multirange functions

Each built-in range family has a two- or three-argument constructor such as `int4range(lower, upper [, bounds])`, and each paired multirange family has a variadic constructor such as `int4multirange(range, ...)`. The generic `multirange(range)` constructor returns the paired multirange identity. Constructor results and generated-column bindings retain the declared subtype across storage and reopen.

User SQL and PL/pgSQL routines may declare `anyrange`, `anymultirange`, `anycompatiblerange`, and `anycompatiblemultirange`. Simple-family calls require one exact built-in range family and link `anyelement` to its subtype; compatible-family calls select a common subtype and its paired range identities without inventing unavailable range-to-range casts. Concrete parameter and return bindings survive nested execution, stored expressions, and reopen, while an unknown call that cannot identify a range family reports SQLSTATE `42804`.

| Functions | Purpose |
| --- | --- |
| `lower`, `upper` | Return the outer lower or upper subtype value, or NULL for an empty or unbounded side |
| `isempty` | Test for an empty range or multirange |
| `lower_inc`, `upper_inc` | Test whether the corresponding finite bound is inclusive |
| `lower_inf`, `upper_inf` | Test whether the corresponding bound is unbounded |
| `range_merge(range, range)` | Return the smallest range covering both inputs |
| `range_merge(multirange)` | Return the smallest range covering every member |
| `multirange(range)`, family multirange constructors | Construct the paired normalized multirange |

The implemented range and multirange operators are `&&` for overlap, `@>` for range-set containment, `<@` for contained-by, and `-|-` for adjacency. These operators require range or multirange operands from the same built-in subtype family; scalar-element containment, complete ordering and arithmetic operators, user-defined range families, and index-backed operator classes remain open compatibility bugs.

```sql execute
SELECT lower('[1,5)'::int4range) AS lower_bound,
       '[1,5)'::int4range && '[4,8)'::int4range AS overlaps,
       '[1,5)'::int4range -|- '[5,8)'::int4range AS adjacent,
       range_merge('{[1,3),[8,10)}'::int4multirange) AS covering_range;
```

## Session and identity functions

Implemented helpers include `current_database`, `current_catalog`, `current_user`, `session_user`, `current_schema`, `current_schemas`, `typeof`, and `pg_typeof`. `current_schema` and `current_schemas` follow the session `search_path`.

### Catalog lookup functions

```text
to_regproc(text) -> regproc
to_regprocedure(text) -> regprocedure
to_regclass(text) -> regclass
to_regnamespace(text) -> regnamespace
to_regrole(text) -> regrole
to_regtype(text) -> regtype
```

The input is a PostgreSQL object name or type spelling. `to_regclass` resolves a relation, `to_regnamespace` resolves a schema, `to_regrole` resolves one global unqualified role, `to_regproc` resolves a unique visible routine name without selecting an overload, `to_regprocedure` requires a routine name followed by an exact input-type signature, and `to_regtype` accepts PostgreSQL type aliases, qualification, typmods, and array bounds while returning the underlying catalog type identity. Object-name components follow PostgreSQL's `reg*` identifier-string rules, so reserved words and non-whitespace punctuation do not require SQL-statement quoting in the text value; quoted components preserve case and doubled quotes, while unquoted components use PostgreSQL case folding and identifier-length clipping. Type spellings use PostgreSQL's dedicated type-name parser.

Each function returns the catalog OID in its declared `reg*` alias; relation, routine, and type lookups use `search_path` when the input is unqualified, while roles are global and qualified role names return NULL. A missing object returns NULL; an ambiguous `to_regproc` name or a signature-less `to_regprocedure` name also returns NULL. An all-digit input uses PostgreSQL's OID input syntax, including its leading-zero octal form, without requiring the OID to identify an existing object, and `-` denotes OID 0. Text output follows the corresponding `reg*` carrier, including visible-name qualification, role identifier quoting, PostgreSQL built-in type aliases, and decimal output for an unresolved nonzero OID.

These lookups do not mutate state. They are strict, stable, parallel-safe, and not leakproof, so a NULL input returns NULL and the functions are rejected in generated-column expressions that require immutability. `pg_catalog.pg_proc` exposes PostgreSQL 18 OIDs 3494, 3479, 3495, 4086, 4093, and 3493 for the functions in the syntax order above, and `information_schema.routines` exposes their exact `reg*` return aliases.

Malformed relation, routine, namespace, or role names are soft lookup failures and return NULL. A cross-database relation, routine, or type name reports SQLSTATE `0A000`; a malformed type specification reports `42601`; and unsupported argument types, names, or arities report `42883`. Qualified namespace and role inputs do not name an object and return NULL. Direct text-to-`regnamespace` casts resolve a schema to its OID carrier, so the result compares directly with OID catalog columns; a missing schema reports `3F000`, malformed OID syntax reports `22P02`, an out-of-range OID reports `22003`, and a malformed or qualified name reports `42602`. Direct text-to-`regrole` and text-to-`regrole[]` casts likewise use the hard input contract: a missing role reports `42704` with the same numeric and name error states; table writes resolve role names to durable OID carriers, so later role removal changes text output to the stored decimal OID rather than corrupting the value.

PostgreSQL does not permit a non-NULL scalar `regrole` constant to be retained in a column default, `CHECK` constraint, generated expression, partition key, view or materialized-view definition, routine parameter default or SQL-standard body, trigger `WHEN` condition, or rule condition or action; such DDL reports `0A000` after applying the hard input errors above. A runtime conversion through `text` or `oid`, a numeric or NULL `regrole` constant, a `regrole[]` constant, and a SQL source-string routine body remain valid because they do not retain that scalar constant dependency.

```sql execute
SELECT to_regclass('pg_catalog.pg_type') AS relation_oid,
       to_regprocedure('casefold(text)') AS routine_oid,
       to_regrole(current_user) AS role_oid,
       to_regtype('integer[]') AS type_oid;
```

## Spatial helpers

`point`, `st_distance`, `st_within`, `st_dwithin`, and `overlaps` provide the implemented point and range operations. UQA Engine does not expose an SQL R-tree index access method, so verify physical behavior for spatial workloads.

## Sequence functions

```text
nextval(sequence regclass)
currval(sequence regclass)
lastval()
setval(sequence regclass, value bigint)
setval(sequence regclass, value bigint, is_called boolean)
```

The sequence argument accepts the PostgreSQL `regclass` input forms, and smaller integer values are implicitly widened to `bigint`; `lastval` takes no arguments. The argument-bearing signatures are strict, so a NULL argument returns NULL without reading or changing sequence state, and `pg_proc` also marks the zero-argument `lastval` signature strict. Unsupported argument types, named notation, or arities report `42883`.

`nextval` returns the next allocated value and establishes the session's `currval` for that sequence. `currval` returns that session-local value. `lastval` returns the current session value of the sequence most recently advanced by `nextval`. Two-argument `setval` is equivalent to `setval(sequence, value, true)`: it stores the value as already called, establishes `currval`, and makes the next `nextval` advance by the sequence increment. `setval(sequence, value, false)` stores an uncalled value, leaves an existing `currval` unchanged, does not establish one when it is undefined, and makes the next `nextval` return the installed value exactly.

Only `nextval` selects the sequence read by `lastval`; calling `setval` for another sequence does not change that selection, while a called-state `setval` for the selected sequence changes the value subsequently returned by `lastval`. The first `nextval` that needs a cache block reserves up to the configured count durably without crossing a bound, returns the block's first value, and serves the remaining values from session-local state. Unused values are abandoned when that session ends or runs `DISCARD SEQUENCES`; a successful sequence-definition change invalidates matching blocks in every session. `setval` invalidates the caller's block, while another session may finish values that it reserved before the call. Values allocated by `nextval` or installed by either `setval` form, along with the affected session `currval` and `lastval` state, are not reclaimed by a failed statement, caught PL/pgSQL exception, transaction rollback, or savepoint rollback when the rollback target retains the same sequence definition. An allocation made against an uncommitted `ALTER SEQUENCE` or `RESTART` definition rolls back with that definition, while any earlier reservation against the restored definition remains preserved; the session `currval` and `lastval` produced by the later call still remain. The durable reservation endpoint and called state survive reopen, while `currval`, `lastval`, and unconsumed cache blocks remain session-local. `DISCARD SEQUENCES` clears both session values and abandons those blocks. Rolling back `DROP SEQUENCE` restores its catalog and session identity, while a committed drop followed by same-named recreation does not transfer either session value to the new object.

`currval` reports `55000` when this session has not established a value for its target. `lastval` reports `55000` before any successful `nextval`, after `DISCARD SEQUENCES`, or when the selected sequence object no longer exists. A permanent sequence cannot be changed in a read-only transaction and reports `25006` before even a cached value is consumed; a temporary sequence remains writable there. Declared `smallint`, `integer`, and `bigint` sequence types use their PostgreSQL bounds, with ascending defaults from `1` through the type maximum and descending defaults from the type minimum through `-1`; explicit minimum and maximum values and cycling are honored. An out-of-bounds `setval` reports `22003`, a nonpositive cache size reports `22023`, advancing a noncycling sequence past its configured bound reports `2200H`, a missing relation reports `42P01`, and a relation of another kind reports `42809`. `pg_sequences` reports the declared type, configured start, minimum, maximum, increment, cycle setting, configured cache size, and the durable reservation endpoint as `last_value`, or NULL while the sequence is uncalled. `pg_proc` exposes the sequence functions' PostgreSQL 18 OIDs, signatures, volatility, parallel safety, strictness, and source identities.

```sql execute
CREATE SEQUENCE manual_setval_sequence START WITH 10;
SELECT setval('manual_setval_sequence', 25, false) AS installed;
SELECT nextval('manual_setval_sequence') AS first_allocated;
SELECT currval('manual_setval_sequence') AS current_value;
SELECT lastval() AS last_allocated;
```

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

`string_agg(value, delimiter [ORDER BY ...])` evaluates both arguments for each input row. It skips NULL values, inserts each retained row's delimiter before that value except for the first retained row, and treats a NULL delimiter as empty. Text inputs return `text`, binary inputs return `bytea`, and an empty input returns NULL. `DISTINCT` considers both arguments, and ordering and bounded spill execution retain each value with its delimiter.

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

Table functions accept a relation alias, positional output-column aliases, and a column definition list where the function contract requires one. PostgreSQL's `ROWS FROM (function_call [AS (column type, ...)], ...) [WITH ORDINALITY] [AS alias (column, ...)]` form preserves each member and its declared columns independently, while the relation alias, positional aliases, and ordinality apply to the complete group.

Each `ROWS FROM` member resolves as an ordinary table-function call against its own arguments and the active `search_path`. The group concatenates member columns in declaration order, emits as many rows as its longest member, and fills columns from exhausted members with SQL NULL. `WITH ORDINALITY` appends one group-wide, one-based `bigint` column after all member columns and resets for each correlated LATERAL invocation.

The group construct does not itself mutate database or session state; each member retains the state and volatility behavior of that function. Range-function groups are implicitly lateral where PostgreSQL permits an earlier `FROM` item to supply an argument, and a `LEFT JOIN LATERAL` null-extends an empty group.

PostgreSQL gives unqualified multi-argument `unnest(array1, array2, ...)` special syntax only in a `FROM` range-function position, including as a member of `ROWS FROM`: it expands to independent unary `pg_catalog.unnest` members, zips them to the longest array, and NULL-pads shorter arrays, so a visible user-defined two-argument `unnest` cannot intercept that syntax. A single unqualified `unnest(array)` remains an ordinary overload-resolved call, a schema-qualified `schema.unnest(array1, array2)` remains one ordinary function call, and `pg_catalog.unnest(array1, array2)` reports undefined function (`42883`) because the catalog has no such ordinary signature. Outside a `FROM` range-function position, a multi-argument `unnest` is also an ordinary function call rather than this syntax transform.

An outer positional alias list may rename any prefix of the concatenated output but cannot contain more names than the group exposes; an oversized list reports invalid column reference (`42P10`). A typed per-member column definition list supplies the required call-site row descriptor for an anonymous `record` or `SETOF record` routine, and execution validates the produced field count and declared source types before applying compatible coercions and type modifiers. Omitting that descriptor from an anonymous record source, attaching one to a scalar or single-output routine, or redundantly attaching one to a known multi-OUT result such as `json_each` reports syntax error (`42601`). Stored views retain every exact member binding across reopen, and missing or ambiguous ordinary member signatures report the corresponding PostgreSQL function-resolution SQLSTATE.

```sql execute
SELECT number, label, sequence
FROM ROWS FROM (
    pg_catalog.generate_series(1, 2),
    pg_catalog.unnest(ARRAY['a', 'b', 'c'])
) WITH ORDINALITY AS rows(number, label, sequence);
```

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
