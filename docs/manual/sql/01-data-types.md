# SQL Data Types

UQA Engine has PostgreSQL-oriented type names mapped to the value carriers implemented by `uqa-core` and the SQL engine. Casts and assignment checks use these engine types rather than PostgreSQL binary storage formats.

## Type matrix

| SQL declaration | UQA Engine representation and notes |
| --- | --- |
| `SMALLINT`, `INTEGER`, `BIGINT` | Distinct declared widths with checked PostgreSQL ranges over a signed 64-bit runtime carrier |
| `INT2`, `INT4`, `INT8`, `INT` | PostgreSQL aliases preserving the corresponding declared width |
| `SMALLSERIAL`, `SERIAL2`, `SERIAL`, `SERIAL4`, `BIGSERIAL`, `SERIAL8` | Width-preserving integer column with generated sequence behavior |
| `OID`, `XID` | Distinct unsigned 32-bit PostgreSQL identities over the integer carrier |
| `REAL`, `FLOAT4` | Distinct single-precision declaration over the floating runtime carrier |
| `FLOAT8`, `DOUBLE PRECISION` | Double-precision declaration over the floating runtime carrier |
| `NUMERIC(p,s)`, `DECIMAL(p,s)` | Exact decimal carrier with declared precision and scale checks |
| `TEXT`, `VARCHAR(n)`, `NAME`, `UUID` | Distinct declared identities over text-compatible carriers; length and UUID input are validated |
| `CHARACTER(n)`, `CHAR(n)` | Blank-padded fixed-length character value; default length is 1 |
| `BOOLEAN`, `BOOL` | Boolean carrier |
| `DATE` | Calendar date |
| `TIME` | Time without timezone |
| `TIMETZ`, `TIME WITH TIME ZONE` | Time with timezone |
| `TIMESTAMP` | Timestamp without timezone |
| `TIMESTAMPTZ`, `TIMESTAMP WITH TIME ZONE` | Timestamp with timezone semantics |
| `INTERVAL` | Calendar/time interval |
| `JSON` | Validated JSON value |
| `JSONB` | Canonical JSON value with JSONB operations |
| `BYTEA` | Byte string |
| `REFCURSOR` | PostgreSQL cursor-name identity over a text-compatible carrier for PL/pgSQL session portals |
| `type[]`, `ARRAY` | Homogeneous array of a supported element type |
| `VECTOR(n)` | One finite fixed-dimensional numeric vector |
| `TENSOR(n)` | A row-level list of finite vectors with fixed element dimension |

## Integer and serial behavior

`SMALLINT`, `INTEGER`, and `BIGINT` retain distinct declared identities and enforce PostgreSQL's signed 16-bit, 32-bit, and 64-bit ranges at casts, writes, schema rewrites, and supported migration boundaries. `OID` casts preserve the source integer width, including PostgreSQL's sign-extension behavior for negative `SMALLINT` and `INTEGER`, while negative `BIGINT` to `OID` raises `22003`; `XID` accepts its PostgreSQL text input but rejects integer and OID cast sources with `42846`.

Serial declarations allocate generated integer identities. Sequence functions `nextval`, `currval`, and `setval` are available, and standalone sequences can be created explicitly. Identity-owned sequence syntax is not implemented.

## Floating point

`REAL`, `FLOAT4`, `FLOAT8`, and `DOUBLE PRECISION` use an IEEE 754 64-bit value. Floating-point equality, ordering, NaN, and infinity behavior should be tested for the application's edge cases. Vector inputs reject non-finite values even when a general floating expression can represent them.

## Exact decimal

`NUMERIC` and `DECIMAL` enforce declared precision and scale. The declaration parser accepts PostgreSQL-shaped precision from 1 through 1000 and scale from -1000 through 1000, while actual values must also fit the engine decimal carrier, which has substantially lower finite precision. Use representative boundary tests when a schema requests more than 28 significant digits.

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

Object key order and formatting are not an application contract for JSONB. Use JSON text only when original textual representation matters.

## BYTEA

`BYTEA` carries arbitrary bytes. `encode` and `decode` convert supported textual encodings, and bytea text input validates hexadecimal digit pairs and legacy escape/octal sequences. When an integer expression has an explicit `SMALLINT`, `INTEGER`, or `BIGINT` source type, its PostgreSQL 18 cast to `BYTEA` emits a signed two-, four-, or eight-byte network-order representation; an unannotated integer expression defaults to `INTEGER`, while boolean, numeric, and floating sources are rejected with PostgreSQL cast SQLSTATEs. `BYTEA`-to-integer casts zero-extend shorter inputs before interpreting the target-width sign bit. Language bindings map byte values to their native byte container, such as Python `bytes` or Node.js `Buffer` and `Uint8Array`.

## REFCURSOR

`REFCURSOR` retains PostgreSQL type identity while carrying a session portal name. PL/pgSQL routines can accept and return it, an explicit text-like cast can name an existing portal, and `pg_typeof(value)::text` reports `refcursor`. An open portal remains fetchable by later routine calls in the same session and transaction until it is closed or the outer transaction ends.

## Arrays

Create arrays with `ARRAY[...]` and inspect them with `array_length`, `array_lower`, `array_upper`, and `cardinality`. Functions also concatenate, append, prepend, remove, replace, sort, reverse, search, format, fill, trim, sample, and unnest arrays.

```sql
SELECT array_length(ARRAY[10, 20, 30], 1) AS length;
SELECT * FROM unnest(ARRAY['sql', 'graph']) AS item(value);
```

Array values are homogeneous under SQL coercion. A SQL `NULL` element remains distinct from an empty array.

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
