# EXPLAIN generic parameter reference

The fixture records 141 independent PostgreSQL 18.4 operations, including unbound GENERIC_PLAN inputs, parameter holes, unresolved types, ordered duplicate/invalid options, mutation descriptions, prepared parameter inference and transaction recovery. Successful plan payloads are omitted because physical plans differ by engine; command tags, result OIDs, parameter OIDs, errors, messages, effects and transaction state are retained. Prepared input hooks remain distinct from ordinary GENERIC_PLAN inference.

Capture used PostgreSQL 18.4 (Debian 18.4-1.pgdg13+1), aarch64, image `sha256:bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25`. Run `PG_EXPLAIN_CONNECTION='dbname=postgres user=postgres' python3 tests/parity/pg18/capture_explain_generic.py` against a disposable PostgreSQL 18 database. The script creates only session-owned temporary and prepared objects.

Capture script SHA-256 is `3052047cabdffb8ff9d4cc781efd74bc966643a80cddf0a27d61d1dde33c7162`; shared capture helper SHA-256 is `30ac0c031ba4776f14eb87c9e9c7c083d30c38d3f72d0788b70da7b9e71bcaea`; expected fixture SHA-256 is `2af3a2f9cbcaabd1e980c764c52ee89d1b381841d585330237bd563bcbf5d79c`.
