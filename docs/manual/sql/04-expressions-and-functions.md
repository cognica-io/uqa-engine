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

The conditions of `WHERE`, `HAVING`, `JOIN ... ON`, a searched `CASE`, `AND`, `OR`, `NOT`, and an aggregate's `FILTER` must be `boolean` or a domain over it, as in PostgreSQL: any other type is `42804` (`argument of WHERE must be type boolean, not type integer`), and a quoted literal whose type is not yet known is read as boolean input, so `'false'`, `'no'`, `'off'`, `'0'`, and their prefixes are false and text that is not a boolean is `22P02`. The operand of a simple `CASE` is compared with each value instead. The retrieval predicates in [Retrieval](06-retrieval.md) are conditions as well.

`LIKE`, `ILIKE`, and `SIMILAR TO` accept `ESCAPE` with a runtime text expression. Omitting the clause uses PostgreSQL's default backslash escape, `ESCAPE ''` disables escaping, `ESCAPE NULL` produces NULL, and every nonempty escape must contain exactly one character. Escaped wildcard and regular-expression metacharacters are treated literally, while escaped alphanumeric characters in `SIMILAR TO` retain the implemented PostgreSQL regular-expression escape behavior.

```sql execute
SELECT value
FROM (VALUES ('a_b'), ('axb')) AS candidates(value)
WHERE value LIKE 'a!_b' ESCAPE '!';
```

## Row and composite values

`ROW(...)` and a parenthesized list of two or more expressions build an anonymous row; casting it to a [composite type](02-ddl.md#composite-types) or assigning it to a column of one coerces each field to its attribute's type, and a row with a different number of fields reports `42846` with PostgreSQL's detail. Text casts to a composite type as `record_in` reads it, and a composite value casts to text as `record_out` writes it, quoting a field that is empty or contains a separator, parenthesis, quote, backslash or whitespace. `(value).field` selects a field: an attribute of a composite value, the `fN` field of an anonymous row, or a column of a relation through its whole-row reference, which `pg_get_viewdef` prints as the column itself. A missing field reports `42703`, as `column t.missing does not exist`, `column "missing" not found in data type pair` or `could not identify column "f3" in record data type`, and field notation on a value that is not composite reports `42809`.

Composite values compare field by field: equality treats two NULL fields as equal, and ordering places NULL fields after all other values, as PostgreSQL's record operators do; anonymous rows compare with SQL's three-valued row comparison instead. `IS NULL` is true for a row or composite value whose fields are all NULL, and `IS NOT NULL` is true when none of its fields is NULL, so a value with both kinds of fields satisfies neither.

```sql execute
SELECT (ROW(1, 'a')).f2 AS second_field,
       ROW(NULL, NULL) IS NULL AS all_null,
       ROW(1, NULL) IS NOT NULL AS none_null,
       ROW(1, NULL) IS NULL AS some_null;
```

## NULL and comparison helpers

| Functions | Purpose |
| --- | --- |
| `coalesce`, `nullif` | NULL selection and conditional NULL |
| `greatest`, `least` | Extremum across scalar arguments |
| `num_nulls`, `num_nonnulls` | Count NULL or non-NULL arguments |

## Function overload resolution

For the implemented fixed-signature built-ins documented below, ordinary expressions and generated expressions use one PostgreSQL 18-style candidate-selection contract. Unqualified calls combine visible SQL user functions with `pg_catalog` candidates according to `search_path`; qualified names, exact and implicit matches, preferred types, unknown-category selection, domain base types, named arguments, defaults, and stored bindings use the same resolver. This contract is limited to the listed implemented signatures and does not imply support for PostgreSQL's complete built-in, polymorphic, operator, cast, or `pg_proc` matrix.

The shared fixed-signature registry covers `abs`, `mod`, `power`, `pow`, `sqrt`, `cbrt`, `casefold`, `reverse`, `md5`, `crc32`, `crc32c`, the documented one-argument length family, `extract`, `date_part`, `date_trunc`, `justify_days`, `justify_hours`, `justify_interval`, `gamma`, `lgamma`, `json_strip_nulls`, `jsonb_strip_nulls`, `to_bin`, `to_hex`, `to_oct`, `to_regproc`, `to_regprocedure`, `to_regclass`, `to_regcollation`, `to_regnamespace`, `to_regrole`, `to_regtype`, `format_type`, the unit and range `random` functions, and the documented UUID generation and extraction functions. Polymorphic array transformations retain their specialized type-substitution path.

Numeric operator syntax (`%`, `^`, unary `+`, `@`, `|/`, and `||/`) selects an operator independently of ordinary function names and `search_path`. Modulo preserves its selected integer or numeric type, power selects numeric or double precision, unary plus and absolute value preserve the selected numeric width, and the square-root and cube-root operators return double precision. Ordinary numeric functions participate in the fixed-signature lookup above; for example, a visible user `mod(integer, integer)` can change `mod(7, 3)` while `7 % 3` still returns `1`. Prepared parameters and stored definitions retain the selected operand types. Undefined operand combinations report `42883`, ambiguous combinations report `42725`, integer result overflow reports `22003`, and invalid floating power or square-root inputs report `2201F`.

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

Stored arithmetic and comparison expressions retain the input conversions of their selected PostgreSQL operator. Numeric/integer expressions print the numeric input cast, and real/integer arithmetic prints the integer operand's selected double-precision conversion. Cross-type integer and real/double operators retain their independently declared operand types. Domain operands retain their base-type relabels, and explicitly written conversions remain explicit through rename and persistent reopening.

Common-type expressions retain the conversions selected during analysis: CASE result arms and COALESCE/GREATEST/LEAST arguments show their selected result type, including domain-to-base relabels. An implicit conversion of an array constructor prints around the array, such as `COALESCE((ARRAY[s])::bigint[], ARRAY[b])`; an explicitly written constructor cast prints its converted elements. Stored SQL-standard routine bodies and generated expressions preserve the same distinction. Array `||` operands retain the selected element promotion in views, generated expressions and SQL-standard bodies. A domain over an array wraps its base array, retaining a separate base conversion only when required; operator expressions keep PostgreSQL's unnamed output label. Reconstruction does not execute the expressions or change CASE/COALESCE branch laziness. The display origin of an otherwise identical conversion does not distinguish inherited CHECK constraints, ON CONFLICT index inference or GROUP BY expression identities.

Named windows retain their declaration order and quoted names in a `WINDOW` clause. Calls print the selected definition as `OVER name`; copied definitions retain their base reference, and an inline specification that matches an earlier raw definition reuses that definition as PostgreSQL does. Unused declarations remain visible and retain their column and type dependencies. Each definition's literal inputs are analyzed once before its effective partition, order and frame are shared by its calls. Reconstructing or reopening the view does not repeat those input effects. Legacy records containing only expanded specifications remain readable and print inline windows because they contain no original names to recover.

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

Both overloads return text reconstructed from the stored index metadata. The one-argument form and a zero `column_number` return the complete CREATE INDEX command without a terminating semicolon. Positive column numbers are one-based and return the selected key expression or included column without its ordering options; negative or out-of-range numbers return an empty string. The full definition preserves uniqueness, the access method, key expressions and order, NULL placement, included columns, `NULLS NOT DISTINCT`, and the partial predicate. Pretty output uses visible relation names and fewer parentheses. Unknown index OIDs and NULL arguments return NULL. Both PostgreSQL signatures are stable, strict, and parallel safe and are exposed in `pg_proc`.

## Role names

`pg_get_userbyid(role_oid oid)` returns the selected role's current name as `name`, including when the caller cannot assume that role. It reads the role catalog without changing database state. NULL input returns NULL; an unknown OID returns `unknown (OID=n)`. The built-in is strict, stable and parallel safe, with PostgreSQL catalog OID 1642. Its result must not be used in an immutable index or generated-column expression.

```sql execute
SELECT pg_get_userbyid(0) AS missing_role,
       pg_get_userbyid(oid) AS role_name
FROM pg_roles WHERE rolname = current_user;
```

## Type display

```sql
SELECT format_type(type_oid, type_modifier);
```

`format_type(type_oid oid, type_modifier integer)` returns the represented catalog type's SQL display name as `text` without changing database state. A NULL modifier omits a modifier; an explicit negative modifier also omits it but preserves PostgreSQL's `bpchar` spelling for unconstrained character types. Character lengths, numeric precision and signed scale, temporal precision, interval fields, and array element modifiers use PostgreSQL's encoded typmods.

A NULL type OID returns NULL, OID zero returns `-`, and an unknown OID returns `???`. Invalid encoded interval field combinations raise `XX000`. The function is stable, parallel safe, and non-strict, with its `(oid, integer) -> text` signature exposed in `pg_proc` as OID 1081. The complete PostgreSQL type and function catalogs remain subject to the compatibility matrix.

The following query returns `character varying(8)`, `numeric(10,2)`, and `timestamp(3) with time zone[]`:

```sql execute
SELECT format_type(1043, 12) AS bounded_text,
       format_type(1700, 655366) AS decimal_type,
       format_type(1185, 3) AS timestamp_array;
```

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

`get_byte(bytea, integer)` returns the unsigned byte at a zero-based position as `integer`. A negative index or an index at or beyond the byte length raises `2202E` with the valid range; either NULL argument returns NULL. The strict, immutable, parallel-safe function is available through `pg_catalog` and accepts the same implicit argument conversions as PostgreSQL.

`substr(bytea, start[, count])` and `substring(bytea FROM start [FOR count])` return raw bytes using one-based positions. A start before one reduces the available count; a start past the end or a zero count returns empty bytea. Negative counts raise `22011`, and NULL arguments return NULL. Binary substring, reverse, length, encoding, hashing, concatenation and position operate on the byte payload, including retained composite fields interpreted as bytea after an attribute type change.

`encode(bytea, format)` and `decode(text, format)` write and read the three formats of PostgreSQL's `encode.c`, whose names compare without regard to case. `hex` writes two lowercase digits for each byte and reads pairs of digits that spaces, tabs and line breaks may separate. `base64` writes a line break after every 76 characters, including one that ends the output, and reads past whitespace; an `=` pads the last group of a sequence. `escape` writes a NUL or a byte with its high bit set as a backslash and three octal digits and doubles a backslash, and reads a backslash followed by another backslash or three octal digits, as the `bytea` input function does. An invalid hex digit reports `22023`, `invalid hexadecimal digit: "g"`, an odd number of digits `22023`, `invalid hexadecimal data: odd number of digits`, an invalid base64 symbol or an incomplete group `22023` with PostgreSQL's message and hint, an invalid escape `22P02`, and an unknown format `22023`, `unrecognized encoding: "name"`.

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

`round(double precision)` and `round(numeric)` return the same type as the selected argument. `round(numeric, integer)` returns a numeric value rounded to the requested number of decimal places; floating-point values need an explicit cast to numeric when a precision argument is supplied. These functions do not change database state. A two-argument call with `real` or `double precision` reports SQLSTATE `42883`, including when the first argument is NULL.

```sql execute
SELECT round(2.71828::numeric, 2) AS rounded,
       round(sin(pi() / 2)::numeric, 6) AS rounded_sine;
```

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
| `array_dims`, `array_ndims`, `array_length`, `array_lower`, `array_upper`, `cardinality` | Dimensions, bounds and element count |
| `array_cat`, `array_append`, `array_prepend` | Construction |
| `array_remove`, `array_replace`, `array_trim`, `array_sample`, `array_sort`, `array_reverse` | Transformation |
| `array_position`, `array_positions`, `array_overlap` | Search and overlap |
| `array_to_string`, `array_fill` | Conversion and construction |
| `unnest` | Expand values as a table function |

The six dimension, bound and element-count routines expose PostgreSQL builtin identities in `pg_proc`, including their `anyarray` argument signatures, immutable/strict behavior and parallel safety. Their names and signatures resolve through `regproc` and `regprocedure`, and their EXECUTE privileges apply to direct calls and stored expressions. Array arguments retain their concrete types; an untyped NULL or string cannot determine the polymorphic array type. Dimension arguments use the declared `integer` signature, including implicit `smallint` widening and rejection of `bigint`. Calls participate in ordinary overload resolution, while `pg_catalog` qualification selects the builtin.

`array_reverse(anyarray)` reverses the first dimension, and `array_sort(anyarray [, descending boolean [, nulls_first boolean]])` orders first-dimension elements while preserving dimensions and lower bounds. The result retains its concrete base-array type, including PostgreSQL's flattening of an array domain to that base type. The two- and three-argument sort overloads accept PostgreSQL's `"array"`, `descending`, and `nulls_first` named notation in declaration-independent order; unknown string literals and bare parameters in Boolean slots receive Boolean context, explicit non-Boolean arguments are rejected, NULL arguments are strict, and an unknown array argument cannot determine the polymorphic type. Unqualified calls participate in normal overload resolution: an exact concrete user-function overload can outrank the polymorphic built-in, an implicit-only user candidate conflicts with a viable built-in, and `pg_catalog` qualification selects the built-in directly. A user overload with incompatible argument names does not hide a matching builtin call. Preparation and stored expressions use the same selection, and stored definitions preserve the written named notation. Explicitly declared non-Boolean option parameters fail during preparation; untyped host scalar parameters still receive Boolean context. Sorting uses PostgreSQL element, nested-array, and record ordering for the implemented types, including the same `json` comparison-function errors, while reversing does not require an element comparator.

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

### JSON extraction operators

Use `value -> key`, `value ->> key`, `value #> path` or `value #>> path`, where `value` is a JSON or JSONB expression. A text `key` selects an object field; an integer `key` selects a zero-based array element, with negative indexes counted from the end. Text keys do not select array indexes, and integer indexes do not select object fields. A `path` is a PostgreSQL `text[]` value whose elements traverse object fields or array indexes; use an array constructor for keys containing commas or other array-literal syntax.

The `->` and `#>` operators return the input JSON or JSONB type. The `->>` and `#>>` operators return text, unquoting JSON strings. Missing keys, out-of-range indexes, incompatible structures and SQL NULL operands return SQL NULL. A present JSON null remains a JSON or JSONB null under `->` and `#>`, but becomes SQL NULL under text extraction. A NULL path element returns SQL NULL; an empty path returns the input value in the operator's result type.

These operators do not change database or transaction state. JSONB extraction results support JSONB equality, including in queries over empty tables, bound-parameter comparisons and generated-column expressions. Equality on JSON values remains an operator-resolution error. Invalid operand types fail operator resolution, and malformed JSON or path-array input fails conversion rather than being treated as a missing field.

```sql execute
SELECT ('{"query":{}}'::jsonb -> 'query') = '{}'::jsonb AS matches,
       ('{"query":null}'::jsonb -> 'query') = 'null'::jsonb AS json_null,
       ('{}'::jsonb -> 'query') IS NULL AS missing,
       '{"query":null}'::jsonb ->> 'query' AS null_text,
       '{"a,b":{"items":[7]}}'::jsonb #> ARRAY['a,b', 'items', '0'] AS nested,
       '{"name":"UQA"}'::jsonb #>> '{name}' AS name;
```

The results are `true`, `true`, `true`, SQL NULL, JSONB `7` and text `UQA`.

### Stripping JSON nulls

`json_strip_nulls(target json [, strip_in_arrays boolean DEFAULT false]) -> json` and `jsonb_strip_nulls(target jsonb [, strip_in_arrays boolean DEFAULT false]) -> jsonb` recursively remove object fields whose value is JSON null. The optional flag retains null array elements when omitted or `false` and removes them when `true`; `target` and `strip_in_arrays` support named notation in declaration-independent order. Both functions are strict, immutable, and parallel safe, do not change database state, preserve the input base return type, and accept domains over their declared argument types. Textual `json` results compact insignificant whitespace while preserving object order, duplicate keys, and numeric lexemes and decoding string escapes; `jsonb` results use normal binary-JSON key and numeric canonicalization. Calls with explicit `text`, the other JSON storage type, a non-Boolean flag, an unknown argument name, or an unsupported arity fail during overload resolution, while malformed unknown JSON input reports invalid JSON syntax.

```sql execute
SELECT json_strip_nulls(strip_in_arrays => true, target => '{"keep":1,"drop":null,"items":[null,{"drop":null}]}'::json);
```

## Temporal functions

| Group | Functions |
| --- | --- |
| Current time | `now`, `transaction_timestamp`, `statement_timestamp`, `clock_timestamp`, `current_date`, `current_time`, `current_timestamp`, `localtime`, `localtimestamp`, `timeofday` |
| Conversion | `to_timestamp`, `to_date`, `to_char` |
| Parts and truncation | `extract`, `date_part`, `date_trunc` |
| Arithmetic and construction | `age`, `make_timestamp`, `make_date`, `make_interval`, `justify_hours`, `justify_days`, `justify_interval` |
| Validation | `isfinite` |

`CURRENT_DATE`, `CURRENT_TIME[(precision)]`, `CURRENT_TIMESTAMP[(precision)]`, `LOCALTIME[(precision)]`, and `LOCALTIMESTAMP[(precision)]` are SQL value expressions. Their result types are `date`, `time with time zone`, `timestamp with time zone`, `time without time zone`, and `timestamp without time zone`, respectively. A precision from zero through six rounds fractional seconds and remains visible in result metadata. SQL value expressions keep their built-in identity even when the search path contains a user function with the same name.

`now()` and `transaction_timestamp()` return the current transaction's start time. The SQL current date/time expressions use the same transaction clock, which survives subsequent statements, savepoints, rollback to a savepoint, and nested execution. `statement_timestamp()` returns the start time of the outer SQL message; statements in one Simple Query message share it. `clock_timestamp()` and `timeofday()` read the wall clock when evaluated. These expressions do not change transaction state. One-argument `age(timestamp)` subtracts its argument from midnight on the transaction's current date, while `age(a, b)` computes `a - b`.

```sql execute
SET TIME ZONE 'UTC';
SELECT pg_typeof(CURRENT_TIME)::text AS time_type,
       pg_typeof(LOCALTIMESTAMP)::text AS timestamp_type,
       now() = CURRENT_TIMESTAMP AS same_transaction_clock,
       CURRENT_TIMESTAMP(3) = CURRENT_TIMESTAMP::timestamptz(3) AS same_precision;
```

The results are `time with time zone`, `timestamp without time zone`, `true`, and `true`. The differential clock transcript uses UTC; session time-zone conversion and display, complete catalog signatures, and precision-reduction diagnostics remain open PostgreSQL compatibility bugs tracked in the [compatibility ledger](09-compatibility.md).

`EXTRACT(field FROM source)` returns `numeric`, and `date_part(field text, source)` returns `double precision`. Each has overloads for `date`, `time`, `time with time zone`, `timestamp`, `timestamp with time zone` and `interval`. Fields are case-insensitive values, including PostgreSQL unit aliases. Numeric extraction preserves exact microseconds and PostgreSQL decimal scales; date_part uses PostgreSQL floating-point evaluation. Interval epoch counts whole stored years as 365.25 days and remaining months as 30 days while retaining the separate day and time fields. Calendar years, centuries and millennia have no year zero in BC output.

Timestamptz calendar fields and timezone fields use the invoking session's `TimeZone`; epoch remains the UTC instant. Timetz uses its stored offset, and its epoch may be outside a single day's range. The timestamptz overloads are stable; other source overloads are immutable. These functions do not change state and return NULL for a NULL argument. Unknown units report `22023`; recognized units unsupported by the selected source report `0A000`. EXTRACT rejects clock fields on a date, while date_part's SQL date wrapper converts the date to midnight timestamp first. Invalid source types fail overload selection. Stored defaults, views, generated expressions and prepared calls retain the selected source type and volatility.

```sql execute
SELECT extract(epoch FROM interval '1 year') AS exact_seconds,
       extract(epoch FROM timestamp '99999-12-31 23:59:59.123456') AS exact_timestamp,
       date_part('hour', date '2024-01-01') AS midnight_hour;
```

The results are `31557600.000000`, `3093527980799.123456` and `0`, with numeric, numeric and double-precision result types.

`date_trunc(unit text, source timestamp)` returns a timestamp, and `date_trunc(unit text, source interval)` returns an interval. The unit is a value naming the retained precision, from millennium through microseconds; PostgreSQL unit aliases are accepted case-insensitively. Interval truncation retains separate month, day and time fields, truncates negative fields toward zero and does not convert months into days or hours into days. Timestamp truncation follows calendar boundaries, including Monday-based weeks and BC centuries and millennia. These functions do not mutate state, and a NULL argument produces NULL. An unrecognized unit reports `22023`; a recognized but unsupported unit reports `0A000`, including `week` for intervals because an interval has no calendar date. A timestamp truncation result before PostgreSQL's minimum date reports `22008` instead of returning an out-of-range value.

```sql execute
SELECT date_trunc('hour', interval '1 day 02:34:56') AS whole_hours,
       date_trunc('week', timestamp '2024-05-19 15:23:45') AS week_start;
```

`date_trunc(unit text, source timestamptz [, timezone text])` returns a timestamptz at the requested local calendar boundary. The two-argument overload uses the invoking session's `TimeZone` and is stable; the explicit-zone overload is immutable and can be used in a stored generated column. The timezone argument accepts case-insensitive IANA names, PostgreSQL's default abbreviations and POSIX timezone rules. Named-zone transitions use bundled IANA 2026b data, including historical offsets, so the result does not depend on the host's installed timezone database. Day and coarser truncation resolves the resulting local date's offset; a DST gap uses the offset before the transition and a fold uses the offset after it. Hour and finer truncation retains the source instant's offset. Any NULL argument returns NULL. An unknown timezone reports `22023` before unit validation, and a final UTC result outside the timestamp range reports `22008`.

```sql execute
SET TIME ZONE 'Asia/Seoul';
SELECT extract(epoch FROM date_trunc('day', timestamptz '2024-01-02 03:04:05+00')) AS session_day,
       extract(epoch FROM date_trunc('day', timestamptz '2024-01-02 03:04:05+00', 'America/New_York')) AS explicit_day;
SET TIME ZONE 'UTC';
```

An interval keeps months, days, and time separately. Multiplying or dividing one by a number scales each field on its own and cascades a fractional month into days at 30 days a month and a fractional day into time at 24 hours a day, never upward, so `interval '1 month' * 0.5` is `15 days`; division divides each field rather than multiplying by the reciprocal, and dividing by zero reports SQLSTATE `22012`. `justify_hours` moves whole 24-hour periods of the time into days, `justify_days` moves whole 30-day periods into months, and `justify_interval` does both so that every field takes one sign; each carry truncates toward zero, so `justify_hours(interval '-25 hours')` is `-1 days -01:00:00`. Adding an interval to or subtracting it from a `time` or `time with time zone` value uses only its time field and wraps within the day. A date, time, timestamp, or interval result outside its type's range reports SQLSTATE `22008`.

```sql execute
SELECT interval '1 day' / 3 AS third,
       interval '1 mon 1 day 1 sec' * 0.3 AS scaled,
       justify_interval(interval '1 mon -00:00:01') AS justified;
```

The results are `08:00:00`, `9 days 07:12:00.3`, and `29 days 23:59:59`.

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

Implemented helpers include `current_database`, `current_catalog`, `current_user`, `session_user`, `current_schema`, `current_schemas`, `typeof`, and `pg_typeof`. `current_schema` and `current_schemas` follow the session `search_path`, and `current_schema` returns NULL when no schema of the path exists. [`current_setting(text [, boolean])`](08-transactions-and-routines.md#set-and-show) reads active session or transaction settings and returns text, with optional NULL for an unknown setting, and `set_config(text, text, boolean)` assigns one. [`pg_sleep`, `pg_sleep_for` and `pg_sleep_until`](08-transactions-and-routines.md#set-and-show) sleep in the session until a cancel or statement timeout ends them.

### Catalog lookup functions

```text
to_regproc(text) -> regproc
to_regprocedure(text) -> regprocedure
to_regclass(text) -> regclass
to_regcollation(text) -> regcollation
to_regnamespace(text) -> regnamespace
to_regrole(text) -> regrole
to_regtype(text) -> regtype
```

The input is a PostgreSQL object name or type spelling. `to_regclass` resolves a relation, `to_regnamespace` resolves a schema, `to_regrole` resolves one global unqualified role, `to_regproc` resolves a unique visible routine name without selecting an overload, `to_regprocedure` requires a routine name followed by an exact input-type signature, and `to_regtype` accepts PostgreSQL type aliases, qualification, typmods, and array bounds while returning the underlying catalog type identity. Object-name components follow PostgreSQL's `reg*` identifier-string rules, so reserved words and non-whitespace punctuation do not require SQL-statement quoting in the text value; quoted components preserve case and doubled quotes, while unquoted components use PostgreSQL case folding and identifier-length clipping. Type spellings use PostgreSQL's dedicated type-name parser.

Each function returns the catalog OID in its declared `reg*` alias; relation, routine, and type lookups use `search_path` when the input is unqualified, while roles are global and qualified role names return NULL. A missing object returns NULL; an ambiguous `to_regproc` name or a signature-less `to_regprocedure` name also returns NULL. An all-digit input uses PostgreSQL's OID input syntax, including its leading-zero octal form, without requiring the OID to identify an existing object, and `-` denotes OID 0. Text output follows the corresponding `reg*` carrier, including visible-name qualification, role identifier quoting, PostgreSQL built-in type aliases, and decimal output for an unresolved nonzero OID.

`to_regcollation` resolves a collation name to a `regcollation` value without changing database state. Built-in names include `"C"`, `"POSIX"`, `"default"`, `pg_c_utf8`, `ucs_basic`, `unicode` and `pg_unicode_fast`; quoted names retain case. NULL input returns NULL. Direct `regcollation` input reports `42704` for an absent collation, while `to_regcollation` returns NULL. Malformed names (`42602`) and invalid numeric input (`22P02` or `22003`) are likewise soft lookup failures. Cross-database names (`0A000`) and names with too many components (`42601`) remain errors in both paths. Scalars and arrays retain OIDs through casts, defaults, prepared parameters, indexes, views and reopen; OID zero prints `-`, and unknown nonzero OIDs print as decimal numbers.

```sql execute
SELECT '"C"'::regcollation AS collation_name,
       to_regcollation('pg_catalog."POSIX"')::oid AS collation_oid;
```

The implemented clock routines `now()`, `transaction_timestamp()`, `statement_timestamp()`, `clock_timestamp()` and `timeofday()`, and the text `lower(text)`/`upper(text)` overloads expose their PostgreSQL identities through the same routine catalog. Their hard `regproc`/`regprocedure` inputs, soft lookup functions, numeric OID output and arrays use the ordinary visibility rules. The overloaded names `lower` and `upper` are ambiguous for hard `regproc` input (`42725`) and return NULL from `to_regproc`; `regprocedure` selects their exact text signature. Stored defaults and view definitions retain the selected OID through search-path changes and durable reopening.

The same catalog exposes `mod(smallint,smallint)`, `mod(integer,integer)`, `mod(bigint,bigint)`, `mod(numeric,numeric)`, the `double precision` and `numeric` overloads of `power`, `pow` and `sqrt`, and `cbrt(double precision)`. Exact `regprocedure` signatures select their PostgreSQL identities; the overloaded names `mod`, `power`, `pow` and `sqrt` are ambiguous as `regproc` (`42725`, or NULL from `to_regproc`), while `cbrt` selects its single identity. Default and view constants preserve those OIDs through shadowing and reopen. Prepared input is reanalyzed when `search_path` changes, following ordinary visibility rules.

A cast of a `reg*` value, or of an array of them, to `text`, `name`, `varchar` or `char` spells the value with its output function, element by element for an array, and then applies the target's length, as PostgreSQL's I/O conversion cast does: `'pg_class'::regclass::varchar(4)` is `pg_c` and `0::regclass::name` is `-`.

These lookups do not mutate state. They are strict, stable, parallel-safe, and not leakproof, so a NULL input returns NULL and the functions are rejected in generated-column expressions that require immutability. `pg_catalog.pg_proc` exposes PostgreSQL 18 OIDs 3494, 3479, 3495, 4195, 4086, 4093, and 3493 for the functions in the syntax order above, and `information_schema.routines` exposes their exact `reg*` return aliases.

Malformed relation, routine, namespace, or role names are soft lookup failures and return NULL. A cross-database relation, routine, or type name reports SQLSTATE `0A000`; a malformed type specification reports `42601`; and unsupported argument types, names, or arities report `42883`. Qualified namespace and role inputs do not name an object and return NULL. Direct text-to-`regnamespace` casts resolve a schema to its OID carrier, so the result compares directly with OID catalog columns; a missing schema reports `3F000`, malformed OID syntax reports `22P02`, an out-of-range OID reports `22003`, and a malformed or qualified name reports `42602`. Direct text-to-`regrole` and text-to-`regrole[]` casts likewise use the hard input contract: a missing role reports `42704` with the same numeric and name error states; table writes resolve role names to durable OID carriers, so later role removal changes text output to the stored decimal OID rather than corrupting the value.

PostgreSQL does not permit a non-NULL scalar `regrole` constant to be retained in a column default, `CHECK` constraint, generated expression, partition key, view or materialized-view definition, routine parameter default or SQL-standard body, trigger `WHEN` condition, or rule condition or action; such DDL reports `0A000` after applying the hard input errors above. A role literal remains prohibited when nested inside another cast or a partition expression. Routine declaration and SQL-standard body errors precede this dependency error. A runtime conversion through `text`, an integer or OID value explicitly cast to `regrole`, a NULL `regrole` constant, a `regrole[]` constant, and a SQL source-string routine body remain valid because they do not retain that scalar constant dependency.

```sql execute
SELECT to_regclass('pg_catalog.pg_type') AS relation_oid,
       to_regprocedure('casefold(text)') AS routine_oid,
       to_regproc('now') AS transaction_clock_oid,
       to_regprocedure('lower(text)') AS text_lower_oid,
       to_regprocedure('mod(integer,integer)') AS remainder_oid,
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

All four value functions acquire RowExclusive on the selected sequence before checking its privileges or using cached values. The lock lasts until the outer transaction ends and survives savepoint rollback; a direct Rust call outside a transaction releases its implicit lock before returning. A call waiting for a sequence definition change retains the original object identity through rename and name reuse, then checks the current definition and authority without advancing the query's ordinary data snapshot. If that object is removed during the wait, the call reports `XX000` rather than using a same-named replacement. This differs from `lastval` finding its selected object already absent before attempting the lock, which reports `55000`.

Only `nextval` selects the sequence read by `lastval`; calling `setval` for another sequence does not change that selection, while a called-state `setval` for the selected sequence changes the value subsequently returned by `lastval`. The first `nextval` that needs a cache block reserves up to the configured count durably without crossing a bound, returns the block's first value, and serves the remaining values from session-local state. Unused values are abandoned when that session ends or runs `DISCARD SEQUENCES`; a successful sequence-definition change invalidates matching blocks in every session. `setval` invalidates the caller's block, while another session may finish values that it reserved before the call. Values allocated by `nextval` or installed by either `setval` form, along with the affected session `currval` and `lastval` state, are not reclaimed by a failed statement, caught PL/pgSQL exception, transaction rollback, or savepoint rollback when the rollback target retains the same sequence definition. An allocation made against an uncommitted `ALTER SEQUENCE` or `RESTART` definition rolls back with that definition, while any earlier reservation against the restored definition remains preserved; the session `currval` and `lastval` produced by the later call still remain. A transaction that changes only a sequence's privileges, owner, name, schema or `OWNED BY` allocates the sequence's values outside itself, as PostgreSQL does, so other sessions draw the following values while it is open and no value is handed out twice whether it commits or rolls back; only a sequence the transaction created, or whose `RESTART`, value options or persistence it changed, allocates inside it. The durable reservation endpoint and called state survive reopen, while `currval`, `lastval`, and unconsumed cache blocks remain session-local. `DISCARD SEQUENCES` clears both session values and abandons those blocks. Rolling back `DROP SEQUENCE` restores its catalog and session identity, while a committed drop followed by same-named recreation does not transfer either session value to the new object.

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

`sum` and `avg` also take `interval` input: the sum adds months, days, and time separately and reports SQLSTATE `22008` when a field overflows, and the average divides that sum by the input count as interval division does, so the average of `1 mon` and `0` is `15 days`.

Aggregates support `DISTINCT`, aggregate-local `ORDER BY`, and `FILTER` where the function shape permits it; on an ordinary function each is `42809` (`FILTER specified, but abs is not an aggregate function`). `min` and `max` compare arrays and record-like map values lexicographically in addition to their scalar inputs.

`mode() WITHIN GROUP (ORDER BY value [ASC | DESC])` returns the most frequent non-NULL value with the input's type, or NULL for empty or all-NULL input. SQL-equal values count together, including signed floating zero, equal intervals and equivalent JSONB representations. When frequencies tie, the first value in the requested ordering wins. Memory and spilled aggregate execution use the same equality and tie rules.

```sql
SELECT department,
       count(*) FILTER (WHERE active) AS active_count,
       string_agg(name, ', ' ORDER BY name) AS names
FROM employees
GROUP BY department;
```

`WITHIN GROUP` calls resolve the function using the direct arguments followed by the ordering expressions. A missing signature is `42883`; a selected scalar function or ordinary aggregate rejects `WITHIN GROUP` with `42809`. Direct and ordered children and the FILTER condition are analyzed before function selection, while implicit input conversions occur only after the selected call passes its modifier checks. Stored definitions preserve this syntax and the selected aggregate identity, and prepared percentile calls accept a typed fraction parameter.

Ordered-set examples use `WITHIN GROUP`:

```sql
SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY latency_ms) AS median
FROM samples;
```

`string_agg(value, delimiter [ORDER BY ...])` evaluates both arguments for each input row. It skips NULL values, inserts each retained row's delimiter before that value except for the first retained row, and treats a NULL delimiter as empty. Text inputs return `text`, binary inputs return `bytea`, and an empty input returns NULL. `DISTINCT` considers both arguments, and ordering and bounded spill execution retain each value with its delimiter.

## Window functions

The window functions are `row_number`, `rank`, `dense_rank`, `percent_rank`, `cume_dist`, `ntile`, `lag`, `lead`, `first_value`, `last_value`, and `nth_value`, and every built-in or registered aggregate can be computed over a window, with `FILTER (WHERE condition)` leaving out the frame rows for which the condition is not true. [Window functions](03-queries-and-dml.md#window-functions) describes peers, frames, and exclusions. As in PostgreSQL, the function is resolved first, with a `WITHIN GROUP` call's ordering expressions after its arguments, and a function that does not exist is `42883`. Calling an ordinary function with `OVER` is `42809`, as is calling `count()` without `*`, an ordered-set aggregate without `WITHIN GROUP`, or any other function with it; an ordered-set aggregate cannot take `OVER` (`0A000`), and neither `DISTINCT`, an aggregate `ORDER BY`, nor `FILTER` on a function that is not an aggregate is implemented for window calls (`0A000`).

## General table functions

| Function | Implemented shape |
| --- | --- |
| `generate_series(start, stop [, step])` | Integer series with two or three arguments |
| `unnest(array)` | One row per array value |
| `regexp_split_to_table(text, pattern)` | One row per split value |
| `string_to_table(text, delimiter)` | One row per split value |
| JSON expansion functions | Key/value or element rows |
| Registered table callbacks | Schema returned by the callback |

`generate_series` has separate two- and three-argument overloads for `integer` and `bigint`; the two-argument form is a distinct function rather than a default argument on the three-argument form. The selected overload determines the output type, so `generate_series(1, 3::bigint)` returns `bigint`. SQL routine bodies, stored views and `ROWS FROM` retain that selected identity across reopening and later search-path shadowing. The corresponding `pg_proc` rows expose the set-returning flag, argument types, result type, row estimate and planner support identity.

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
