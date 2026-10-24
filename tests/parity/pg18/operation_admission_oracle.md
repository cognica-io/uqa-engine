# Operation admission and recovery reference

Captured on 2026-10-24 from PostgreSQL 18.4 (Debian 18.4-1.pgdg13+1), aarch64 Linux, using the independent `uqa-pg18-projection-reference` container and its libpq implementation. The capture creates a session-local temporary sequence and an ordinary table inside a transaction that is rolled back; no durable object survives the connection.

Run `capture_operation_admission_oracle.py` with `PG_ADMISSION_CONNECTION` set to an explicit reference connection. The script SHA-256 is `ec2a2ae9754807d829fb430d047f02a1f5aa6d21604a99a5902c4fdfd5f82043`; the captured `operation_admission_oracle.expected.json` SHA-256 is `976c778e157b1c10aa573a079c3370dee2eb9a5ca07e8978687338fcf1fb6411`.

The fixture records undefined `lastval`/`currval`, out-of-range `setval`, exhausted `nextval`, rejection of subsequent work, savepoint recovery and transaction rollback. It also records sequence values surviving rollback and read-only mutation failure followed by the same recovery sequence. Numeric transaction states are libpq's idle (`0`), in-transaction (`2`) and failed (`3`) states.

`crates/uqa-engine/src/transactions/tests/direct_admission.rs` compares SQL observations with every recorded field on memory and native SQLite. Direct sequence APIs return string errors, so those comparisons cover the actual diagnostic, value and transaction state without inventing a SQLSTATE. Direct row, prepared-row, metadata, definition, mapped, storage and graph mutation boundaries compare read-only and subsequent-abort SQLSTATEs and recovery states; their operation-specific diagnostic text is not equated with PostgreSQL's DELETE message.
