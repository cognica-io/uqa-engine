# SQL Data Types

UQA Engine has PostgreSQL 18-compatible type names mapped to the value carriers implemented by `uqa-core` and the SQL engine. Casts and assignment checks use these engine types rather than PostgreSQL binary storage formats.

## Type matrix

| SQL declaration | UQA Engine representation and notes |
| --- | --- |
| `SMALLINT`, `INTEGER`, `BIGINT` | Distinct declared widths with checked PostgreSQL ranges over a signed 64-bit runtime carrier |
| `INT2`, `INT4`, `INT8`, `INT` | PostgreSQL aliases preserving the corresponding declared width |
| `SMALLSERIAL`, `SERIAL2`, `SERIAL`, `SERIAL4`, `BIGSERIAL`, `SERIAL8` | Width-preserving integer column with generated sequence behavior |
| `OID`, `XID` | Distinct unsigned 32-bit PostgreSQL identities over the integer carrier |
| `INT2VECTOR`, `OIDVECTOR` | Distinct catalog-vector carriers with one zero-based dimension, including empty values; atomic elements inside an outer SQL array |
| `REGTYPE` | Type-catalog OID over the integer carrier; cast to text or use PostgreSQL result formatting for its visible SQL name |
| User-defined domains | A distinct catalog type over its base value, with [declaration defaults and conversion-time constraints](02-ddl.md#domain-declarations-and-deletion) |
| User-defined enum types | A distinct catalog type whose values keep an immutable label identity; [labels, their order and renames](#enum-types) come from the [enum declaration](02-ddl.md#enum-types) |
| `REAL`, `FLOAT4` | IEEE 754 single-precision inputs, arithmetic, and sums over a widened floating runtime carrier |
| `FLOAT8`, `DOUBLE PRECISION` | Double-precision declaration over the floating runtime carrier |
| `NUMERIC(p,s)`, `DECIMAL(p,s)` | Exact decimal carrier with declared precision and scale checks |
| `TEXT`, `VARCHAR(n)`, `NAME`, `UUID` | Distinct declared identities over text-compatible carriers; length and UUID input are validated |
| `CHARACTER(n)`, `CHAR(n)` | Blank-padded fixed-length character value; default length is 1 |
| `BOOLEAN`, `BOOL` | Boolean carrier |
| `DATE` | Calendar date |
| `TIME[(p)]` | Time without timezone, with optional fractional-second precision |
| `TIMETZ[(p)]`, `TIME[(p)] WITH TIME ZONE` | Time with timezone and optional fractional-second precision |
| `TIMESTAMP[(p)]` | Timestamp without timezone and optional fractional-second precision |
| `TIMESTAMPTZ[(p)]`, `TIMESTAMP[(p)] WITH TIME ZONE` | Timestamp with timezone semantics and optional fractional-second precision |
| `INTERVAL [fields] [(p)]` | Calendar/time interval with optional stored-field restriction and fractional-second precision |
| `JSON` | Validated JSON value |
| `JSONB` | Canonical JSON value with JSONB operations |
| `BYTEA` | Byte string |
| `REFCURSOR` | PostgreSQL cursor-name identity over a text-compatible carrier for PL/pgSQL session portals |
| `type[]`, `ARRAY` | Homogeneous array of a supported element type |
| `INT4RANGE`, `INT8RANGE`, `NUMRANGE`, `DATERANGE`, `TSRANGE`, `TSTZRANGE` | PostgreSQL built-in range identities with canonical text I/O over their scalar subtype |
| `INT4MULTIRANGE`, `INT8MULTIRANGE`, `NUMMULTIRANGE`, `DATEMULTIRANGE`, `TSMULTIRANGE`, `TSTZMULTIRANGE` | PostgreSQL built-in multirange identities whose members are normalized, ordered, and merged |
| `VECTOR(n)` | One finite fixed-dimensional numeric vector |
| `TENSOR(n)` | A row-level list of finite vectors with fixed element dimension |

## Integer and serial behavior

`SMALLINT`, `INTEGER`, and `BIGINT` retain distinct declared identities and enforce PostgreSQL's signed 16-bit, 32-bit, and 64-bit ranges at casts, writes, schema rewrites, and supported migration boundaries. `OID` casts preserve the source integer width, including PostgreSQL's sign-extension behavior for negative `SMALLINT` and `INTEGER`, while negative `BIGINT` to `OID` raises `22003`; `XID` accepts its PostgreSQL text input but rejects integer and OID cast sources with `42846`.

Text input to an integer type reads what PostgreSQL's `int2in`, `int4in` and `int8in` read: surrounding whitespace, an optional sign, and decimal digits or `0x`, `0o` and `0b` digits that single underscores may separate, so `'0x1F'::integer` is 31 and `' 1_000 '::bigint` is 1000. A text outside the type's range reports `22003`, `value "40000" is out of range for type smallint`, and any other text `22P02`.

Serial declarations allocate generated integer identities. Sequence functions `nextval`, `currval`, `lastval`, and `setval` are available, and standalone sequences can be created explicitly. Identity-owned sequence syntax is not implemented.

## Catalog vectors

`int2vector` and `oidvector` accept space-separated integer text and preserve their declared identity through domains, assignment, prepared parameters and persistence. Elements are non-NULL signed 16-bit integers or unsigned 32-bit OIDs, respectively. Text input creates exactly one dimension with lower bound zero; an empty text vector has bounds `[0:-1]`, length zero and cardinality zero. `anyarray` functions preserve the vector type while changing its array metadata: `trim_array` and `array_sample` return nonempty results with lower bound one, or dimensionless empty results. The `anycompatiblearray` functions `array_cat`, `array_append`, `array_prepend`, `array_remove` and `array_replace` return ordinary arrays, with the common element type selected from their arguments; for example, appending a `smallint` to an `int2vector` returns `smallint[]`, while appending an `integer` returns `integer[]`. An outer `int2vector[]` or `oidvector[]` treats each vector as one element, even when its contents are empty or have different lengths.

`int2vector` equality and ordering include array dimensions and lower bounds after comparing elements lexicographically. `oidvector` scalar operators ignore lower bounds and compare length before elements; they reject dimensionless arrays with `42804`. SQL equality, grouping, joins and unique indexes use those same declared semantics. PostgreSQL resolves `MIN` and `MAX` over these types to its array aggregate, which compares elements before dimensions; consequently `MIN(oidvector)` can choose a different value from ascending scalar `ORDER BY`. A cast to the corresponding `smallint[]` or `oid[]` preserves bounds, including the empty vector dimension. Element-converting casts can change empty-array shape: `''::int2vector::integer[]` is dimensionless, while the binary-compatible `''::oidvector::integer[]` retains `[0:-1]`.

Casting an ordinary array to either vector type fails with `42846`, including a NULL array. Comparing a vector directly with an ordinary array fails with `42883`. Invalid integer input reports `22P02`; out-of-range elements report `22003`. Converting a dimensionless vector to text reports `42804` with `array is not a valid int2vector` or `array is not a valid oidvector`; array introspection and JSON conversion can still consume that value.

Ordered and DISTINCT aggregates, sorting, and unique or nonunique B-tree keys propagate vector comparison errors. An index can retain one dimensionless `oidvector` without comparing it; a later key comparison fails with `42804`. Composite keys compare earlier fields first. UPDATE retains index entries only when all indexed inputs keep their stored representations, including expression, predicate and included-column dependencies. SQL equality alone does not establish unchanged storage: signed zero, numeric scale and interval fields can require new index comparisons even when the values compare equal.

```sql execute
SELECT '1 2'::int2vector AS items,
       array_dims(''::oidvector) AS empty_bounds,
       array_dims(ARRAY[''::int2vector, '1 2'::int2vector]) AS outer_bounds;
```

The result is `1 2`, `[0:-1]`, and `[1:2]`. Rust exposes `Value::LegacyVector(LegacyVectorValue)` with an explicit `LegacyVectorKind`; it is distinct from an ordinary `Value::Array` or untyped `Value::List`. See the [upgrade contract](../reference/10-upgrading.md#040-catalog-vector-carriers) for existing stored values.

## Floating point

`REAL` (`FLOAT4`) converts inputs directly to IEEE 754 single precision. Widening the stored value to the engine's 64-bit carrier preserves that rounded value; casting it to `DOUBLE PRECISION` does not restore discarded precision. `FLOAT8` (`DOUBLE PRECISION`) uses double precision. Arrays, domain bases, column assignment, and declared prepared parameters apply the same conversions. PostgreSQL text output and character casts use the declared floating width.

The `+`, `-`, `*`, and `/` operators use single precision when both operands are `REAL`. Mixing `REAL` with an integer, `NUMERIC`, or `DOUBLE PRECISION` selects double precision. `SUM(real)` rounds at each single-precision addition, while `AVG(real)` returns double precision. Aggregate `ORDER BY` controls the addition order. Grouped, window, and spilled aggregate state retain the selected width.

Numeric comparisons apply the operand conversions selected by PostgreSQL's operator signatures. For example, `1.0 > 0` is true, and `9007199254740993::bigint = 9007199254740992::double precision` is also true because the integer is rounded to double precision for that comparison. Comparing the same integer against `9007199254740992::numeric` is false. Internal ordered and hash keys compare the represented values exactly after SQL coercion; they do not replace a binary float with its shortest display text.

Invalid floating text reports `22P02`; overflow or underflow outside the representable range reports `22003`. Representable subnormal values, signed zero, NaN, and infinity are retained. Division by zero reports `22012`, except that a NaN numerator remains NaN. Vector inputs still reject non-finite values.

```sql execute
SELECT 16777217::real::double precision AS rounded_input,
       (16777216::real + 1::real)::double precision AS real_sum,
       16777216::real + 1 AS mixed_sum;
```

The result is `16777216`, `16777216`, and `16777217`, respectively. The [compatibility ledger](09-compatibility.md) tracks the remaining complete floating-point regression and I/O matrix.

## Exact decimal

`NUMERIC` and `DECIMAL` enforce declared precision and scale. Declarations and casts accept precision from 1 through 1000 and scale from -1000 through 1000. Invalid precision, scale or modifier count reports PostgreSQL's `22023` diagnostic during type analysis, even for NULL inputs, empty results or an unselected CASE branch. A value whose rounded magnitude does not fit the declared precision reports `22003` `numeric field overflow` with PostgreSQL's DETAIL, `A field with precision 3, scale 1 must round to an absolute value less than 10^2.`, in casts, `INSERT` and `UPDATE` alike. Unconstrained finite values support up to 131,072 digits before the decimal point and 16,383 fractional digits. Decimal storage and comparisons preserve their exact base-10 value, including values beyond binary floating-point precision.

```sql
CREATE TABLE invoices (
    invoice_id INTEGER PRIMARY KEY,
    amount NUMERIC(18, 2) NOT NULL CHECK (amount >= 0)
);
```

Use exact decimal for financial values. Do not substitute floating point where exact base-10 arithmetic is an invariant.

## Text and character types

`TEXT`, `VARCHAR(n)`, `NAME`, and `UUID` retain distinct declared identities while using text-compatible carriers. `VARCHAR(n)` rejects overlength assignment except for discarded trailing spaces, explicit casts follow PostgreSQL truncation behavior, `NAME` preserves its catalog identity, and UUID input is validated and emitted canonically.

`CHARACTER(n)` pads shorter values with ASCII spaces to its fixed width. Comparisons follow the implemented character coercion behavior; normalize at application boundaries when exchanging data with another database.

## Temporal types

Temporal types support comparisons, extraction, truncation, construction, formatting, parsing, age calculation, and current-time functions. The default session timezone is `UTC`, and `SET timezone` changes session behavior where timezone conversion applies.

Text reaches a temporal type through PostgreSQL's input functions. `DATE`, `TIMESTAMP` and `TIMESTAMPTZ` read ISO 8601 dates (`2024-01-02`, `20240102`, `2024/01/02`, `2024.01.02`) and month-day-year dates whose first field has fewer than three digits (`01-02-2024`), an optional time of day with `am` or `pm`, a sixtieth second and `24:00:00`, fractional seconds rounded past six digits, and a UTC offset (`+05`, `+0530`, `+05:30:15`, `Z`, `UTC`, `GMT`); `TIME` reads the time of day and ignores a date before it or an offset after it, and `TIMETZ` keeps the offset. The special values resolve against the transaction start, as `GetCurrentTransactionStartTimestamp` supplies it to the input functions, so `'now'::timestamp = now()::timestamp` and `'today'::date = current_date` hold within one statement: `now` names the current instant for every type, `today`, `tomorrow` and `yesterday` midnight of those days and accept a time of day after them (`'tomorrow 10:00+02'::timestamptz`), `epoch` is `1970-01-01 00:00:00+00` for `DATE`, `TIMESTAMP` and `TIMESTAMPTZ`, and `allballs` is `00:00:00` for `TIME` and `TIMETZ`; `'epoch'::time`, `'today'::time`, `'allballs'::timestamp`, `'now 10:00'::timestamp` and `'now'::interval` report `22007`. A default of `'now'` or `'today'` is read when the table is created and stored as the creation time, as `cookDefault` stores the coerced constant, and `pg_get_expr` prints it through the output function (`'2026-10-05 12:22:51.549461'::timestamp without time zone`), as it prints every stored temporal constant (`'01:00:00'::interval` for a default written `'1 hour'`). The input diagnostics are PostgreSQL's: `22007` `invalid input syntax for type time: "x"` for text that is not a value, `22008` `date/time field value out of range: "25:00"` for a field past its range with the hint `Perhaps you need a different "DateStyle" setting.` when a month or day field is the cause, `22008` `timestamp out of range: "294277-01-01"` past the type's range, `22009` `time zone displacement out of range` for an offset past 15 hours or with a minute or second field past its range, `22023` `time zone "nowhere/zone" not recognized` for a zone name that is not an offset, and `22015` `interval field value out of range: "1-13"` for an interval field or quantity that does not fit, while `'1 2'`, `'ago'` and `'1 dayx'` are `22007` as `DecodeInterval` rejects them. An operator reads an `unknown` operand with the operand type it selects, so `timestamp '2024-01-01' - '1 day'` reads `'1 day'` as a timestamp and reports `22007`, and a generation expression selects the same operator (`ts + '1 day'` reads an interval and is stored as `(ts + '1 day'::interval)`, `d + '1'` on a date is `42725`). `time + date` and `date + time` produce a `TIMESTAMP` and `timetz + date` and `date + timetz` a `TIMESTAMPTZ` at the instant the offset names, as `datetime_pl` and `datetimetz_pl` do. Years before the common era read and print with the `BC` era (`'0001-01-01 BC'::date`, and `'0001-01-01'::date - 1` is `0001-12-31 BC`), `AD` leaves the year as written, there is no year zero (`'0000-01-01'` is `22008`), and years past 9999 print without a sign (`99999-01-01`). A written cast of a literal in a stored expression prints as the constant the input function read, with `format_type`'s spelling of the cast's type: `CHECK (t < '11:00'::time(3))` prints `(t < '11:00:00'::time(3) without time zone)` and `'y'::bytea` prints `'\x79'::bytea`. A value of a non-temporal, non-text type has no cast to a temporal type: `1::time` reports `42846` `cannot cast type integer to time without time zone`. Named time zones and abbreviations after the time, the infinite values, month names in dates, the ISO 8601 interval format and timestamps past the year 262143 are not read yet; see the compatibility manifest.

Timestamp input also checks PostgreSQL's minimum Julian instant, `4714-11-24 00:00:00 BC`. Earlier `timestamp` values report `22008`; `timestamptz` applies its UTC offset before checking that boundary, so `4714-11-23 12:00:00-12 BC` is accepted as exactly the minimum instant.

Date-only timestamp inputs can carry an adjacent positive offset or `Z`, such as `2024-01-01+05:30` or `2024-01-01Z`. A negative offset after a hyphenated date requires a separating space (`2024-01-01 -05:30`). Compact numeric offsets use hours and minutes; seconds require the colon form. Zone displacement validation precedes calendar validity, and BC leap-day validation uses the final calendar year.

Casting a typed `DATE` or `TIMESTAMP` to `TIMESTAMPTZ` interprets the local date or time in the invoking session's `TimeZone`; a date supplies midnight. Gaps use the pre-transition offset and folds use the post-transition offset. Implicit argument and assignment casts use the same conversion, including prepared statements and stored defaults evaluated after a timezone change. This conversion is stable, so an immutable generated expression cannot acquire it implicitly.

```sql execute
SET TIME ZONE 'Asia/Seoul';
SELECT extract(epoch FROM timestamp '2024-01-02 03:04:05'::timestamptz) AS utc_seconds;
RESET TimeZone;
```

When an input is read, temporal coercion, comparison and date or timestamp range bounds use the same transaction clock as an explicit cast. Simple Query batches refresh that clock after `COMMIT`, `ROLLBACK` and their `AND CHAIN` forms; previously read constants retain their values.

`TIME(p)`, `TIMESTAMP(p)`, and their timezone variants retain a fractional-second precision from 0 through 6 in column declarations, casts, function-source column definitions, array elements, result metadata, and persistent catalogs. Values are rounded when a cast or assignment applies the declaration. Rounding at the end of a day can produce `24:00:00`, which remains distinct from `00:00:00` as a time value. `pg_attribute.atttypmod` and `information_schema.columns.datetime_precision` expose the declared modifier after reopen.

`TIME` compares the stored time without wrapping the day, so `24:00:00` sorts after `00:00:00`. `TIMETZ` first compares UTC-adjusted time without day wrapping, then the original timezone offset in PostgreSQL's seconds-west order. Consequently, `12:00:00+00` sorts after and is unequal to `13:00:00+01`, even though their UTC-adjusted times match. DISTINCT, grouping, uniqueness and ordered indexes use the same equality and order. Time arithmetic retains its separate day-wrapping behavior.

`INTERVAL` supports the fields `YEAR`, `MONTH`, `DAY`, `HOUR`, `MINUTE`, and `SECOND`, plus `YEAR TO MONTH`, `DAY TO HOUR`, `DAY TO MINUTE`, `DAY TO SECOND`, `HOUR TO MINUTE`, `HOUR TO SECOND`, and `MINUTE TO SECOND`. The least significant field determines truncation: for example, `INTERVAL HOUR TO MINUTE` preserves years, months, days, hours, and minutes while discarding seconds. `INTERVAL(p)` and ranges ending in `SECOND(p)` round fractional seconds. `information_schema.columns.interval_type` exposes an explicit field restriction, including its precision when present.

```sql execute
SELECT '23:59:59.9995'::time(3) AS midnight,
       '1 year 2 mons 3 days 04:05:06.789'::interval hour to minute AS whole_minutes;
```

```sql
CREATE TABLE events (
    event_id INTEGER PRIMARY KEY,
    event_date DATE NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

SELECT event_id, date_trunc('day', starts_at) AS day
FROM events;
```

Use `TIMESTAMPTZ` for instants and `TIMESTAMP` for timezone-independent wall-clock values. Store the original zone identifier separately when it is a business value.

## Range and multirange types

The six PostgreSQL built-in range families are implemented as declared SQL identities: `INT4RANGE` / `INT4MULTIRANGE`, `INT8RANGE` / `INT8MULTIRANGE`, `NUMRANGE` / `NUMMULTIRANGE`, `DATERANGE` / `DATEMULTIRANGE`, `TSRANGE` / `TSMULTIRANGE`, and `TSTZRANGE` / `TSTZMULTIRANGE`. Range literals use PostgreSQL bound notation, including unbounded and `empty` values, and multirange literals use braces around comma-separated range members.

```sql execute
SELECT '[1,4]'::int4range AS canonical_integer_range,
       '{[1,3),[3,5),[10,12)}'::int4multirange AS normalized_multirange;
```

Discrete integer and date ranges canonicalize inclusive upper bounds and exclusive lower bounds to PostgreSQL's inclusive-lower, exclusive-upper form when the adjacent subtype value exists. Multiranges discard empty members, order members, and merge overlapping or adjacent members. The declared range or multirange identity survives storage, query planning, generated expressions, schema rewrites, foreign-table boundaries, reopen, and `pg_typeof`; `pg_type` and `pg_range` expose the corresponding PostgreSQL 18 built-in OIDs and subtype relationships.

This implemented surface is limited to PostgreSQL's six built-in range families and text-form values. User-defined range types, binary range I/O, range indexes and exclusion-index planning, and complete comparison ordering remain open compatibility bugs.

## Enum types

An enum type is a catalog-defined list of labels. A value's text is exactly one of its type's labels: input is case-sensitive, keeps surrounding spaces, and reports `22P02` for any other text. Values order by the labels' declaration order, not by their spelling, and every comparison operator, `ORDER BY`, `DISTINCT`, grouping, joins, `min` and `max` use that order. Distinct enum types never compare with each other or with text; such comparisons report `42883` like PostgreSQL. A string literal or untyped parameter compared with an enum value, listed in `IN`, used as a `BETWEEN` bound or passed where a function expects the enum type is converted to that type once, as PostgreSQL's parse analysis converts it, so an unknown label is rejected even when no row is read.

```sql execute
CREATE TYPE ticket_state AS ENUM ('new', 'triaged', 'closed');
CREATE TABLE tickets (id integer, state ticket_state, history ticket_state[]);
INSERT INTO tickets VALUES (1, 'closed', '{new,closed}'), (2, 'new', NULL), (3, 'triaged', '{new}');
SELECT id, state, state > 'new' AS progressed, enum_range(NULL::ticket_state) AS all_states
FROM tickets
WHERE state IN ('new', 'closed')
ORDER BY state;
```

Casts from the string types (`text`, `varchar`, `char`, `name`) convert through the label, and casts to them produce the label; there are no other casts to or from an enum type (`42846`). Output functions of containers and text or JSON builders use the current label: `||`, `concat`, `concat_ws`, `format`, `quote_literal`, `quote_nullable`, `to_json`, `to_jsonb`, `row_to_json`, `array_to_json`, `json_build_object`, `json_build_array`, their JSONB forms, `json_agg`, `jsonb_agg` and the JSON object aggregates. Arrays of an enum type use its generated array type, named like PostgreSQL's `_name` with a numeric suffix when that name is taken.

`enum_first(anyenum)`, `enum_last(anyenum)` and `enum_range(anyenum)` read only their argument's type, so a typed NULL such as `NULL::ticket_state` selects the type; `enum_range(lower, upper)` returns the labels between two values inclusively, treating a NULL bound as open and returning an empty array when the lower bound follows the upper bound. `enum_first` and `enum_last` of a type without labels report `55000`. `enum_cmp`, `enum_eq`, `enum_ne`, `enum_lt`, `enum_le`, `enum_gt`, `enum_ge`, `enum_smaller` and `enum_larger` compare two values of one type, `hashenum` and `hashenumextended` hash the label's OID, and SQL routines may declare `anyenum` parameters and results. An `anyenum` argument must be an enum type rather than a domain over one, and every `anyenum` argument of a call must have the same type.

A value stores an immutable label key, not its text. `ALTER TYPE ... RENAME VALUE` therefore changes the text of existing values without rewriting them, and `ALTER TYPE ... ADD VALUE ... BEFORE | AFTER` places a new label between existing ones without changing any stored order. A label that a transaction adds to a type the same transaction did not create cannot be read, compared or returned until the transaction commits (`55P04`); labels of a type created in the same transaction are usable at once. `pg_enum.enumsortorder` follows PostgreSQL's float4 midpoint positions, including its renumbering when a midpoint cannot be represented, while value order is unaffected. Embedded clients receive enum values as labels: see [host result labels](../reference/02-rust-engine-api.md#enum-labels-in-results).

## JSON and JSONB

JSON values must be syntactically valid. JSONB provides canonical object behavior and containment, path, update, insertion, deletion, key, and expansion functions.

```sql
CREATE TABLE records (
    id INTEGER PRIMARY KEY,
    payload JSONB NOT NULL
);

INSERT INTO records (id, payload)
VALUES (1, '{"kind":"manual","tags":["sql","rust"]}'::jsonb);

SELECT jsonb_extract_path_text(payload, 'kind') AS kind
FROM records;
```

Object key order and formatting are not an application contract for JSONB. Use JSON text only when original textual representation matters. JSONB numeric comparisons use numeric magnitude, so zero precedes positive fractions and follows negative values regardless of scale or exponent spelling. The same rule applies to numeric members of arrays and objects. Equality, grouping, uniqueness and ordered index lookups agree on these numeric values.

## BYTEA

`BYTEA` carries arbitrary bytes. `encode` and `decode` convert supported textual encodings. Text input reads PostgreSQL's two formats, as `byteain` does, whether a cast, a literal or parameter written to a column, or `COPY` supplies it: the hex format, `\x` followed by pairs of hex digits that spaces, tabs and line breaks may separate, as in `'\x01 02'`, and the escape format, in which a backslash introduces another backslash or three octal digits, as in `'a\\b\001'`. An odd number of hex digits reports `22023`, `invalid hexadecimal data: odd number of digits`, another character among them `22023`, `invalid hexadecimal digit: "g"`, and any other backslash in the escape format `22P02`, `invalid input syntax for type bytea`. A typed text or integer expression is not written to a `BYTEA` column without a cast and reports `42804`. When an integer expression has an explicit `SMALLINT`, `INTEGER`, or `BIGINT` source type, its PostgreSQL 18 cast to `BYTEA` emits a signed two-, four-, or eight-byte network-order representation; an unannotated integer expression defaults to `INTEGER`, while boolean, numeric, and floating sources are rejected with PostgreSQL cast SQLSTATEs. `BYTEA`-to-integer casts zero-extend shorter inputs before interpreting the target-width sign bit. Language bindings map byte values to their native byte container, such as Python `bytes` or Node.js `Buffer` and `Uint8Array`.

## REFCURSOR

`REFCURSOR` retains PostgreSQL type identity while carrying a session portal name. PL/pgSQL routines can accept and return it, an explicit text-like cast can name an existing portal, and `pg_typeof(value)::text` reports `refcursor`. An open portal remains fetchable by later routine calls in the same session and transaction until it is closed or the outer transaction ends.

## Arrays

Create arrays with `ARRAY[...]` and inspect them with `array_length`, `array_lower`, `array_upper`, and `cardinality`. Functions also concatenate, append, prepend, remove, replace, sort, reverse, search, format, fill, trim, sample, and unnest arrays.

```sql
SELECT array_length(ARRAY[10, 20, 30], 1) AS length;
SELECT * FROM unnest(ARRAY['sql', 'graph']) AS item(value);
```

Array values are homogeneous under SQL coercion. A SQL `NULL` element remains distinct from an empty array.

An unknown string cast to an array of domains is read by the array and element-domain input functions during analysis. Element CHECK and NOT NULL constraints therefore apply even under `WHERE false`, in an unselected CASE arm, or when a statement is prepared. The converted array retains its domain identity, dimensions and lower bounds, and execution does not repeat those input checks. A prepared array constant keeps its accepted value after domain constraints change; an explicitly typed text expression or parameter is converted against the current domain when it executes. A NULL array differs from an array containing a NULL domain element. Column and routine defaults, stored CHECK expressions and ALTER TABLE USING retain the same typed input values; default assignment and column backfill preserve their source type. A scalar domain cast retains its outer domain checks for execution after its base input is read, including a NULL cast in a default.

## VECTOR and TENSOR

The dimension must be a positive integer:

```sql
CREATE TABLE embeddings (
    id INTEGER PRIMARY KEY,
    document_embedding VECTOR(384),
    token_embeddings TENSOR(384)
);
```

A vector input must contain exactly `n` finite numeric values. A tensor contains zero or more vectors, each with exactly `n` finite values. KNN uses cosine similarity, and tensor matching assigns the row its best element score.

Only one physical IVF or HNSW index may own a vector column at a time. Brute force remains available when no physical vector index exists.

## NULL

SQL `NULL` represents an unknown or absent value and follows three-valued logic. Use `IS NULL` and `IS NOT NULL`, not equality with `NULL`.

```sql
SELECT id
FROM records
WHERE payload IS NOT NULL;
```

`COALESCE` selects the first non-NULL value, and `NULLIF` produces NULL when its two arguments compare equal. Aggregate functions normally ignore NULL inputs except where their stated contract differs; `count(*)` counts rows and `count(expression)` counts non-NULL values.

## Casts and type inspection

Use either cast syntax:

```sql
SELECT CAST('42' AS INTEGER) AS value;
SELECT '42'::INTEGER AS value;
SELECT pg_typeof(42) AS type_name;
```

Conversions can fail on invalid syntax, overflow, non-finite vector values, dimension mismatch, decimal precision or scale violation, invalid JSON, or incompatible assignment. Treat a conversion failure as an input error instead of silently substituting a default.

A string literal or untyped parameter has PostgreSQL's `unknown` type until its context selects one. Every operator gives it the operand type of the selected operator and reads it with that type's input function before the statement runs, as `make_op` and `coerce_type` do, so `true = 't'`, `'a'::bytea = 'a'` and `ARRAY[1, 2] = '{1,2}'` compare typed values, `1 + '1'` reads `'1'` as an integer, and `1.5 + 'x'` and `true = 'x'` report `22P02` `invalid input syntax for type numeric: "x"` and `invalid input syntax for type boolean: "x"`. The operator is selected as `oper_select_candidate` and `func_select_candidate` select it: when the candidates disagree on the category of an `unknown` position, the known operand's type is assumed for it and the one candidate every operand reaches by implicit casts is taken, so `time '10:00' + '1 hour'` selects `time + interval` through the implicit cast from `time` to `interval` and `rk(1, 'x')` selects `rk(numeric, numeric)` over `rk(numeric, boolean)`, while `date '2024-01-01' + 'x'`, `-'1'`, `'1' + '2'` and a prepared `$1 + '1'` report `42725` `operator is not unique` with the hint `Could not choose a best candidate operator. You might need to add explicit type casts.`. `||` exists for a text operand with any non-array operand, two arrays, two `bytea` values and two `jsonb` values, so `1 || 2` and `'x'::bytea || 1` report `42883` `operator does not exist` with the hint `No operator matches the given name and argument types. You might need to add explicit type casts.`, and an `unknown` operand of `||` takes the typed operand's type: `'x'::bytea || 'y'` joins the bytes as `\x7879`, `'{}'::jsonb || '{"a":1}'` concatenates `jsonb`, `ARRAY[1] || '{2}'` appends the array, and `'x'::bytea || 'y'::text` concatenates the output texts as `anytextcat` does. An `IN` list compares the tested value and every item at their common type when they have one, and otherwise compares each item through its own `=` operator, as `transformAExprIn` does, in queries and in `INSERT`, `UPDATE` and `DELETE` alike; each `BETWEEN` bound takes the type of its comparison, `op ANY (array)` and `op ALL (array)` give an untyped array literal the array type of the selected operator, and `NULLIF` and `IS DISTINCT FROM` resolve their equality operator the same way. The inputs that `VALUES`, `CASE`, `COALESCE`, `UNION`, `ARRAY` and `GREATEST` unify take PostgreSQL's common type, as `select_common_type` selects it: types of one category meet at the first input's type unless that type is not the category's preferred type and coerces implicitly to a later input's type that does not coerce back, and every input must then coerce implicitly to the type selected. `oid` is the preferred type of the numeric category, so `oid` and an OID alias type such as `regclass` meet at `oid` when `oid` comes first and at the alias otherwise; an integer and an OID alias type meet at the alias; `time` and `timetz` meet at `timetz`; and `CASE` considers its `ELSE` result before its `THEN` results. A conflict names its construct as PostgreSQL does: inputs of different categories report `42804` `UNION types integer and date cannot be matched`, `VALUES types ...`, `CASE types ...`, `COALESCE types ...`, `ARRAY types ...`, `GREATEST types ...`, `LEAST types ...`, `IN types ...`, `JOIN/USING types ...` or `CYCLE types ...`, and an input of the selected type's category without an implicit cast to it reports `42846` `VALUES could not convert type regclass to regtype`, `CASE/WHEN could not convert type ...` for a `CASE`. Inputs that are all `unknown` resolve to `text`, in set operations and subquery outputs as in `CASE`, `COALESCE`, `ARRAY`, `GREATEST` and `VALUES`, so `SELECT NULL UNION SELECT NULL` has a `text` column and an integer column cannot take `DEFAULT (CASE WHEN random() < 2 THEN NULL END)`. An `unknown` literal among typed inputs is read by the selected type's input function before the statement runs, as `coerce_to_common_type` reads it, so `CASE WHEN true THEN 1 ELSE 'x' END`, `COALESCE(1, 'x')`, `1 IN (1, 'x')`, `GREATEST(1, 'x')` and `VALUES (1), ('x')` report `22P02` `invalid input syntax for type integer: "x"` even when the literal's branch would not be evaluated. The `anycompatible` array functions `array_append`, `array_prepend`, `array_cat`, `array_remove`, `array_replace`, `array_position` and `array_positions` convert untyped arguments to the element or array type chosen by their typed arguments.

Ordinary statement analysis resolves FROM sources before target-list inputs and reads target expressions in order, before constant optimization. For example, `SELECT 'absent'::regclass FROM missing_source` reports the missing source relation, while `SELECT 'absent'::regclass, missing_column` reports the missing regclass relation. The same input boundary applies within nested queries and command queries; typed runtime casts keep their evaluation semantics. A declared cursor retains the catalog metadata needed for a regclass reference even when it does not scan that relation.

```sql execute
SELECT 5 IN ('5', '6') AS in_list,
       '5' = ANY (ARRAY[5, 6]) AS any_array,
       nullif(5, '5') IS NULL AS nullif_equal,
       array_position(ARRAY[1, 2, 1], '1', '2') AS position_from_two;
```
