# Legacy encrypted container

`chacha20poly1305-0.10-zstd.uqa` is an 889-byte encrypted compressed SQLite fixture created by `ManagedConnection::open_compressed_encrypted` at commit `94d825d5e62a3ca6e5842316d371ffcc18573278`, with `chacha20poly1305` 0.10.1, `SQLiteCompressionOptions::zstd()` and the public test key `legacy-fixture-public-key`. It contains no user data. The regression reads its authenticated chunks, modifies the database using the current cipher dependency, and reopens it.

The generating SQL was:

```sql
CREATE TABLE saved (id INTEGER PRIMARY KEY, value TEXT);
INSERT INTO saved VALUES (7, 'legacy encrypted container');
```
