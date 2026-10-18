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

## Standalone release installation

Starting with 0.6.0, GitHub releases attach `uqa-cli-<version>-<target>.tgz` for Linux and macOS, `.zip` archives for Windows, matching `.sha256` files, and `install-usql.sh` / `install-usql.ps1`. Each system has x86_64 and ARM64 builds on native runners. The standalone executable includes Nori, Kuromoji, Arrow and DuckDB; no Python or Rust installation is needed. Linux builds target GNU libc on Ubuntu 24.04, so use Ubuntu 24.04 or a compatible newer distribution; these are not musl/Alpine binaries.

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall#installation) installed, Cargo users can download the same official binary without compiling UQA:

```sh
cargo binstall uqa-cli --strategies crate-meta-data
usql
```

The installed command is `usql`, while the crate name is `uqa-cli`. `--strategies crate-meta-data` selects the project's release artifacts and fails if no matching binary exists; it cannot fall back to compilation or third-party builds. To select a release, replace `uqa-cli` with `uqa-cli@<version>`. The 0.5.4 and earlier releases do not have these standalone assets. Cargo's built-in `cargo install` still compiles source. Use the attached installer below when you want it to verify the accompanying SHA-256 file; cargo-binstall downloads through HTTPS but does not consume these checksum sidecars.

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

## Editing and output

The Rust shell accepts multiline SQL, PostgreSQL dollar and escape strings, quoted identifiers and nested comments. Highlighting and bracket matching use the complete query buffer; parentheses inside quoted text never block Enter. Tab completion reads the live catalog and accepts unqualified, schema-qualified and quoted relation names, columns, variables, settings and paths. It does not insert SQL keywords into strings or comments. `NO_COLOR` disables color; `\set HIGHLIGHT off` disables it for the current session.

`\timing [on|off]` and `\x [on|off|auto]` accept explicit settings; omitting the argument toggles the setting. Expanded `auto` uses `\pset columns` as its width threshold. `\pset` lists the current printing settings. `\pset format aligned`, `unaligned`, `csv`, `html` or `json` selects a renderer; `\a` and `\H` toggle unaligned and HTML output. `\t [on|off]` controls tuples-only output, `\f separator` changes the unaligned separator, and `\C title` sets a table title. Other settings include `expanded`, `null`, `footer`, `border`, `columns`, `fieldsep`, `recordsep`, `fieldsep_zero`, `recordsep_zero`, `csv_fieldsep` and `pager`.

SQL owns typed text output, including booleans, arrays, enums, retained composites and catalog identity types. Rendering preserves column order and duplicate labels. Aligned output uses Unicode display widths and escapes control characters; terminal CSV also neutralizes terminal controls. File CSV and unaligned output preserve data bytes according to the selected format. JSON contains ordered `columns` and positional `rows`; each non-NULL cell is SQL text and NULL is JSON `null`, preserving exact decimal and integer spelling. Like psql CSV, the default NULL marker and an empty string both produce an empty CSV field; use native COPY when a stream must distinguish them according to COPY's quoting rules.

`\o [file]` opens a fresh output file or restores standard output. `\g [file]` and `\gx [file]` optionally redirect only that submission. An interactive terminal uses `PSQL_PAGER`, then `PAGER`, when `\pset pager on` needs another screen; `always` forces the pager and `off` disables it. Pager and editor commands accept quoted executable arguments and are launched directly, without a shell.

## Backslash commands

| Command | Action |
| --- | --- |
| `\q`, `\quit`, `\exit` | Exit |
| `\help`, `\?` | Show client-command help |
| `\h [command]` | Show SQL syntax help and the canonical manual link |
| `\open path`, `\connect [path]`, `\c [path]` | Open or reconnect to an embedded database; `:memory:` selects a fresh memory database |
| `\new` | Replace the current engine with a fresh in-memory engine |
| `\reset` | Reopen the current database and reset session state |
| `\where`, `\conninfo` | Print the current database location or connection information |
| `\timing [on\|off]` | Configure timing output |
| `\expanded [on\|off\|auto]`, `\x [on\|off\|auto]` | Configure expanded display |
| `\pset [option [value]]` | Inspect or change printing settings |
| `\a`, `\H`, `\t`, `\f`, `\C` | Output format, tuples, separator and title shortcuts |
| `\o [path]` | Redirect query output or restore standard output |
| `\p`, `\r` | Print or clear the pending query buffer |
| `\g [path]`, `\gx [path]` | Submit the pending query, or repeat the previous query when the buffer is empty |
| `\gset [prefix]` | Store exactly one returned row in shell variables; SQL NULL unsets a variable |
| `\set [name [value ...]]`, `\unset name` | List, set or remove shell variables |
| `\if`, `\elif`, `\else`, `\endif` | Conditional script execution |
| `\echo`, `\qecho` | Print arguments to standard output or query output; `-n` omits the newline |
| `\dt [pattern]`, `\d [pattern]` | List tables or describe matching relations; bare `\d` lists relations |
| `\di`, `\dv`, `\dn`, `\df`, `\da` | List indexes, views, schemas, routines or aggregates; each accepts a pattern |
| `\du`, `\dg`, `\l`, `\dx`, `\dp`, `\z` | Inspect roles, databases, extensions or privileges; each accepts a pattern |
| `\ds [sequence]`, `\stats table` | Inspect sequences or optimizer statistics |
| `\graphs`, `\analyzers` | List UQA named graphs or custom analyzers |
| `\dfs`, `\dft` | List foreign servers or foreign tables |
| `\i file`, `\ir file`, `\run file` | Execute a script; `\ir` resolves relative to its including script |
| `\e [file [line]]`, `\w file` | Edit or write the query buffer |
| `\history [clear]`, `\s [file]` | Show, clear or save command history |
| `\copy ...` | Stream a table or query to/from a local file using native COPY text/CSV |
| `\watch [seconds] [i=seconds] [c=count] [m=rows]` | Repeat the current query until interrupted, the count is reached or fewer than the minimum rows are returned |
| `\migrate-python-db source destination` | Migrate a legacy Python database |

Catalog patterns support `*`, `?`, regular expressions, schema qualification and case-preserving double quotes. In unquoted UTF-8 patterns, only ASCII letters fold to lowercase; non-ASCII identifiers retain their exact case, as in PostgreSQL psql. `\dg` lists roles and `\da` lists aggregates; use `\graphs` and `\analyzers` for the previous UQA operations. `\dS` includes system relations. The existing UQA `\dF` alias remains equivalent to `\dft`. `\analyzers` lists custom persistent names; `SELECT * FROM list_analyzers()` also includes built-ins. See [text analyzers](06-text-analyzers.md) for analyzer configuration and field binding.

## Variables, scripts and files

Shell variables expand as `:name` for raw SQL, `:'name'` for a quoted SQL literal, `:"name"` for a quoted identifier and `:{?name}` for an existence Boolean. Quoted SQL strings, identifiers, dollar bodies and comments do not expand variables. In meta commands, substituted values stay within one argument: `\set copied :value` preserves spaces, while `\echo :'value'` retains the requested SQL literal quotes. Single-quoted meta arguments accept doubled apostrophes and backslash escapes; double quotes remain part of ordinary meta arguments. `\gset` validates the complete returned row before changing variables, preserves positional columns and rejects a result with zero or multiple rows.

```text
\set item 41
SELECT :item + 1 AS answer
\gset result_
\if :{?result_answer}
SELECT :'result_answer' AS answer \g
\endif
```

`\set ON_ERROR_STOP on` stops script processing at the first error. `\set AUTOCOMMIT off` opens an explicit transaction before an ordinary SQL command; use `COMMIT` or `ROLLBACK` to finish it. Included files share the same Engine session and variables. Includes are limited to 32 active files, reject cycles and require balanced conditional blocks within a file; conditionals are limited to 64 levels and pending SQL to 16 MiB. Script errors include their file and line. A final SQL statement without a semicolon executes at EOF, as in psql.

`\e` uses `PSQL_EDITOR`, then `EDITOR`, then `VISUAL`, with `vi` on Unix or `notepad` on Windows as the fallback. Without a filename it creates a private temporary file and removes it after use. History uses `UQA_HISTORY` when set, otherwise `~/.cognica/uqa/.usql_history`; `HISTCONTROL` accepts `none`, `ignorespace`, `ignoredups` and `ignoreboth`, and `HISTSIZE` bounds retained entries.

Client file COPY keeps SQL parsing and text/CSV encoding in Engine. Failed exports preserve an existing destination file; imports retain native COPY transaction semantics. COPY filenames are literal client paths and do not undergo shell-variable substitution or command execution:

```text
\copy items TO 'items.csv' WITH (FORMAT CSV, HEADER)
\copy items FROM 'items.csv' WITH (FORMAT CSV, HEADER)
```

Ctrl-C clears pending interactive input or cancels an executing SQL command. In piped input or script-file mode, interruption stops the remaining input and exits with status 3, matching psql; an interrupted interactive `\i` returns to the prompt. Catalog completion errors leave the prompt available for transaction recovery. Cancellation leaves PostgreSQL transaction recovery rules intact: an aborted explicit transaction requires rollback. LISTEN notifications use the current session's bounded inbox and are shown while the editor is idle as well as after commands; no idle notification polling runs without committed subscriptions. Reconnection keeps client printing settings and variables but replaces the SQL session.

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
