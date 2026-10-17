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

The Rust CLI enables the bundled `nori` and `kuromoji` analyzers by default, matching the Python-installed command. A custom `cargo build -p uqa-cli --no-default-features` omits both and rejects requests for those analyzers. Enabling `--features nori,kuromoji` forwards the features to the existing Engine and analysis owners.

Current source builds include the memory foreign-data wrapper. Add `--features arrow` for native Arrow IPC execution or `--features duckdb` for native DuckDB execution; neither feature enables the other. Foreign-table definitions, `\d`, `\dft`, `\dfs` and completion use SQL catalog metadata and remain available for omitted native handlers.

## Standalone release installation (development source)

Starting with the next release, GitHub releases attach `uqa-cli-<version>-<target>.tgz` for Linux and macOS, `.zip` archives for Windows, matching `.sha256` files, and `install-usql.sh` / `install-usql.ps1`. Each system has x86_64 and ARM64 builds on native runners. The standalone executable includes Nori, Kuromoji, Arrow and DuckDB; no Python or Rust installation is needed. Linux builds target GNU libc on Ubuntu 24.04, so use Ubuntu 24.04 or a compatible newer distribution; these are not musl/Alpine binaries.

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall#installation) installed, Cargo users can download the same official binary without compiling UQA:

```sh
cargo binstall uqa-cli --strategies crate-meta-data
usql
```

The installed command is `usql`, while the crate name is `uqa-cli`. `--strategies crate-meta-data` selects the project's release artifacts and fails if no matching binary exists; it cannot fall back to compilation or third-party builds. To select a release, replace `uqa-cli` with `uqa-cli@<version>`. The existing 0.5.4 and earlier releases do not have these standalone assets. Cargo's built-in `cargo install` still compiles source. Use the attached installer below when you want it to verify the accompanying SHA-256 file; cargo-binstall downloads through HTTPS but does not consume these checksum sidecars.

Download the installer from the desired [GitHub release](https://github.com/cognica-io/uqa-engine/releases), then run it. Each attached installer is pinned to that release version and downloads the matching architecture's archive over HTTPS, verifies its SHA-256 checksum, and installs into a user directory without changing shell configuration:

```sh
sh install-usql.sh
# Optional destination or previously downloaded archive and checksum:
sh install-usql.sh --bin-dir "$HOME/bin" --archive-dir ./downloads
```

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\install-usql.ps1
# Optional destination or previously downloaded archive and checksum:
pwsh -NoProfile -ExecutionPolicy Bypass -File .\install-usql.ps1 -BinDir "$HOME\bin" -ArchiveDirectory .\downloads
```

The defaults are `$HOME/.local/bin` on Linux/macOS and `%LOCALAPPDATA%\UQA\bin` on Windows. Add the chosen directory to `PATH` if the installer reports that it is missing. The Windows example sets the execution policy only for that PowerShell process, leaving persistent policy unchanged. The archive also contains license notices and a manifest recording the source commit, target and enabled features. Installers verify downloads before replacing an existing executable; failed downloads or checksums preserve the previous installation.

## Invocation

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
