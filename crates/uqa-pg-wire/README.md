# uqa-pg-wire

`uqa-pg-wire` is the UQA Engine crate for network-independent PostgreSQL protocol 3.0 through 3.2 message parsing and encoding.

The crate decodes startup, simple-query, extended-query, COPY, function-call, password/SASL, cancellation, and encryption-negotiation frontend messages and encodes the corresponding backend messages. `Bind` and `FunctionCall` expose one shared implementation of PostgreSQL's zero, one, or one-per-value format-code expansion, including binary parameters and binary result selection. `AuthenticationExchange` validates cleartext, MD5, GSS, SSPI, and SASL message order and decodes the context-dependent frontend `p` message without owning credentials or cryptographic verification. `CancelKey` validates the protocol-version limits and provides bounded prefix composition for layered middleware while preserving the downstream opaque secret.

The crate intentionally does not own sockets, TLS, credential storage, SQL execution, transaction policy, pooling, or recovery. The single integration test target includes in-process malformed-peer and byte-exact contracts; the opt-in Docker matrix under `tests/parity/pg18/clients` runs pinned psycopg, pgx, and node-postgres clients against both PostgreSQL 18.4 and a server assembled from these codecs.

Applications should depend on `uqa-engine` or `uqa-client`. See the [repository README](https://github.com/cognica-io/uqa-engine) and the [manual](https://github.com/cognica-io/uqa-engine/blob/main/docs/manual/README.md).
