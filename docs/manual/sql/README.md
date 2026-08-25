# Supported SQL

UQA Engine parses PostgreSQL-oriented SQL with `libpg_query`, compiles it into UQA Engine AST and plans, and executes it inside the embedded engine. Syntax acceptance does not imply complete PostgreSQL server behavior; this manual states the implemented contract.

## Reference map

| Document | Coverage |
| --- | --- |
| [Data types](01-data-types.md) | Scalar, temporal, JSON, array, vector, tensor, NULL, and conversion behavior |
| [DDL](02-ddl.md) | Schemas, tables, constraints, indexes, views, sequences, and foreign tables |
| [Queries and DML](03-queries-and-dml.md) | `SELECT`, joins, CTEs, grouping, windows, set operations, and mutations |
| [Expressions and functions](04-expressions-and-functions.md) | Operators, scalar functions, aggregates, windows, and table functions |
| [Analyzer SQL](05-analyzers.md) | Analyzer JSON, lifecycle functions, field phases, GIN binding, and diagnostics |
| [Retrieval SQL](06-retrieval.md) | Full-text, vector, hybrid, learned, and model retrieval functions |
| [Graph SQL and Cypher](07-graph.md) | Named graphs, Cypher, RPQ, traversal, and centrality |
| [Transactions and routines](08-transactions-and-routines.md) | Transactions, prepared statements, session settings, SQL functions, PL/pgSQL, and procedures |
| [Compatibility](09-compatibility.md) | PostgreSQL alignment, deliberate differences, limits, and unsupported syntax |

## Statement summary

| Family | Implemented statements |
| --- | --- |
| Query | `SELECT`, `VALUES`, `WITH`, `WITH RECURSIVE`, `UNION`, `INTERSECT`, `EXCEPT` |
| Mutation | `INSERT`, `UPDATE`, `DELETE`, `MERGE`, `TRUNCATE` |
| Relation DDL | `CREATE TABLE`, `CREATE TABLE AS`, `ALTER TABLE`, `DROP TABLE`, `CREATE VIEW`, `CREATE OR REPLACE VIEW`, `DROP VIEW` |
| Namespace DDL | `CREATE SCHEMA`, `DROP SCHEMA` |
| Index DDL | `CREATE INDEX`, `DROP INDEX` for B-tree, GIN, IVF, and HNSW |
| Sequence DDL | `CREATE SEQUENCE`, `ALTER SEQUENCE` |
| Foreign data | `CREATE SERVER`, `CREATE FOREIGN TABLE` |
| Session and diagnostics | `SET`, `SHOW`, `DISCARD`, `LOAD`, `ANALYZE`, `EXPLAIN` |
| Transactions | `BEGIN`, `START TRANSACTION`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`, `ROLLBACK TO SAVEPOINT` |
| Prepared SQL | `PREPARE`, `EXECUTE`, `DEALLOCATE` |
| Routines and roles | `CREATE FUNCTION`, `CREATE PROCEDURE`, `CREATE OR REPLACE`, implemented `ALTER FUNCTION`/`PROCEDURE`/`ROUTINE` attributes and ownership, routine `GRANT`/`REVOKE`, `CREATE`/`ALTER`/`DROP ROLE`, `CREATE USER`, `SET ROLE`, `RESET ROLE`, `DROP FUNCTION`, `DROP PROCEDURE`, `DO`, `CALL` |

Unsupported statement shapes fail with an error instead of silently discarding clauses.

## Function contract format

Function and feature references use one consistent contract order so a reader or LLM can distinguish syntax from executable behavior:

1. **Syntax** gives the implemented call shape rather than every PostgreSQL overload with the same name.
2. **Arguments** states each type and whether an operand is an SQL identifier, an expression, or a value.
3. **Result** states scalar type, support behavior, or table columns and types.
4. **Effects** states catalog, index, session, transaction, or persistence changes; read-only functions say so explicitly.
5. **Errors** states validation and unsupported shapes that fail.
6. **Example** is the smallest complete SQL example that demonstrates the contract.

Names in code font are canonical. Do not derive alternate spellings by splitting initialisms or component names: for example, analyzer JSON uses `html_strip` and `ascii_folding` exactly. A compatibility alias is documented only when the implementation registers it.

Arguments described as relation or column identifiers are SQL grammar, not string values. Parameters such as `$1` can replace values but cannot replace those identifiers. Relation-producing functions compose through `FROM`, aliases, joins, subqueries, and CTEs; they are not scalar expressions unless a function contract explicitly says otherwise.

Every fenced SQL block in this manual is checked by the `manual_sql_examples` integration test. Plain `sql` blocks must compile, `sql execute` blocks also run in source order against one in-memory engine per Markdown file, and `sql compile-fail` blocks must fail compilation. Unknown classifications and empty SQL blocks fail the test.

## Parameters

Use PostgreSQL positional placeholders:

```sql
SELECT id, title
FROM articles
WHERE status = $1 AND created_at >= $2
ORDER BY id;
```

`Engine::sql` receives a parameter slice in placeholder order. A placeholder is an expression value, not an identifier or SQL fragment. Bind untrusted values; select table, column, and ordering identifiers from trusted application metadata.

Vectors and tensors have explicit `SQLParam::vector` and `SQLParam::tensor` constructors. Scalar arrays can also be expressed with `ARRAY[...]` in SQL.

## Names and schemas

Unquoted identifiers follow PostgreSQL-style case folding. Double quotes preserve an identifier's spelling and can contain dots without turning them into name separators. UQA Engine supports schema-qualified names and `search_path`, but not cross-database three-part references.

```sql
CREATE SCHEMA app;
SET search_path TO app, public;
CREATE TABLE tasks (id INTEGER PRIMARY KEY);
SELECT current_schema();
```

## Literals and comments

The parser accepts ordinary SQL string, numeric, Boolean, NULL, byte, array, and dollar-quoted literals supported by the implemented expression compiler. Use `--` for a line comment and `/* ... */` for a block comment. Dollar quoting is especially useful for Cypher and routine bodies.

```sql
SELECT $$text containing 'quotes'$$ AS body;
```

## Ranked query columns

Retrieval predicates produce virtual columns such as `_score` and document support identities. Project `_score` only in a query that establishes ranked support, and order explicitly:

```sql
SELECT id, title, _score
FROM articles
WHERE text_match(body, 'embedded SQL')
ORDER BY _score DESC, id ASC
LIMIT 20;
```

## Diagnostics

Use `EXPLAIN` with optional `ANALYZE`, `VERBOSE`, and `FORMAT TEXT` or `FORMAT JSON`. `ANALYZE` collects optimizer statistics for all tables or one table; options and column lists are not implemented.

```sql
ANALYZE articles;
EXPLAIN (ANALYZE, FORMAT JSON)
SELECT id FROM articles WHERE status = 'open';
```

The virtual `information_schema` and `pg_catalog` relations expose UQA Engine catalog state for inspection and compatibility. They are not a complete PostgreSQL system catalog.
