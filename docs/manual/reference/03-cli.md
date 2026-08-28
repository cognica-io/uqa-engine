# usql CLI

`usql` is the interactive and batch SQL client included in `uqa-cli`.

## Start the client

Install the `uqa` package from PyPI to install both the Python binding and the `usql` command into the same Python environment:

```sh
python -m pip install uqa
usql
```

When building from this repository instead, run the Rust binary directly:

```sh
cargo run -p uqa-cli --bin usql
```

Common forms are:

```text
usql
usql --db mydata.uqa
usql script.sql
usql --db mydata.uqa script.sql
usql -c "SELECT 1 AS ready"
usql migrate-python-db source.db destination.uqa
```

When running through Cargo, place CLI arguments after `--`:

```sh
cargo run -p uqa-cli --bin usql -- --db mydata.uqa -c "SELECT 1"
```

Without `--db`, the client uses a new in-memory engine. A script is executed before the REPL starts when standard input is a terminal. `-c` executes its SQL and exits.

## Encrypted databases

```text
usql --db encrypted.uqa --key secret
usql --db encrypted.uqa --key-file ./database.key
UQA_KEY=secret usql --db encrypted.uqa
```

Key precedence is `--key`, then `--key-file`, then `UQA_KEY`. Interactive sessions prompt when an encrypted database is opened without a supplied key. The CLI detects plain, encrypted, compressed, and compressed-encrypted containers when opening a database path.

Command-line secrets can be visible to process inspection and shell history. Prefer a protected key file or a deployment-specific secret injection mechanism.

## Input and output behavior

- End SQL statements with `;`.
- Multiline statements remain buffered until they are complete.
- `\timing` toggles elapsed time reporting.
- `\x` toggles expanded row display.
- `\o path` appends query output to a file; bare `\o` restores standard output.
- `\run path.sql` executes a SQL file in the current session.
- `\history` prints history and `\history clear` clears it.

## Backslash commands

| Command | Action |
| --- | --- |
| `\q`, `\quit`, `\exit` | Exit |
| `\help`, `\?`, `\h` | Show command help |
| `\open path` | Open a persistent database, with format detection |
| `\new` | Replace the current engine with a fresh in-memory engine |
| `\reset` | Reopen the current database and reset session state |
| `\where` | Print the current database location |
| `\timing` | Toggle timing output |
| `\expanded`, `\x` | Toggle expanded display |
| `\o [path]` | Redirect output or restore standard output |
| `\dt` | List local and foreign tables |
| `\d table` | Describe a local or foreign table |
| `\di` | List indexes |
| `\stats [table]` | Show optimizer statistics |
| `\ds` | List sequences |
| `\dg` | List named graphs |
| `\dfs` | List foreign servers |
| `\dft` | List foreign tables |
| `\da` | List custom engine-catalog text analyzers |
| `\run file` | Execute a SQL file |
| `\migrate-python-db source destination` | Migrate a legacy Python database |

Aliases shown by `\help` are accepted in addition to this compact list.

`\da` uses `Engine::list_named_analyzers` and therefore lists custom persistent names, not the four built-ins. Use `SELECT * FROM list_analyzers()` to see the combined built-in and custom SQL-visible set. Analyzer creation, JSON configuration, field binding, and diagnostics are covered in [Text analyzer pipelines](06-text-analyzers.md).

## Script execution and transactions

A script can contain multiple SQL statements. One `-c` command string follows PostgreSQL simple-query boundaries: statements share an implicit transaction until `COMMIT` or `ROLLBACK` closes it, `BEGIN` promotes that implicit transaction without committing preceding work, and `SAVEPOINT`, `RELEASE SAVEPOINT`, or `ROLLBACK TO SAVEPOINT` requires a block made explicit by an earlier `BEGIN`; subsequent statements after a completed `COMMIT` or `ROLLBACK` start another atomic implicit segment, including when the command began inside a pre-existing transaction. File and interactive input execute each submitted terminated statement independently, so use explicit transaction SQL when the entire script must be atomic:

```sql
BEGIN;

CREATE TABLE ledger (
    id INTEGER PRIMARY KEY,
    amount NUMERIC(18, 2) NOT NULL
);

INSERT INTO ledger (id, amount) VALUES (1, 25.00);

COMMIT;
```

If a statement fails inside an explicit transaction, inspect the error and issue `ROLLBACK` before continuing.

## Catalog inspection with SQL

Backslash commands are convenient, but catalog relations are queryable too:

```sql
SELECT table_schema, table_name
FROM information_schema.tables
ORDER BY table_schema, table_name;
```

The virtual `information_schema` and `pg_catalog` relations support tooling and portable inspection. They expose UQA Engine state, not a complete PostgreSQL server catalog.
