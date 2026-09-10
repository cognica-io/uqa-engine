# PostgreSQL TCP server

The development workspace includes the unpublished `uqa-pg-server` crate. It exposes UQA Engine sessions through PostgreSQL's Simple Query protocol on a TCP listener. It is separate from the [HTTP Engine API](09-http-engine.md).

## Start a listener

```sh
cargo run -p uqa-pg-server -- --database /tmp/uqa-pg-server.db --trust
psql 'host=127.0.0.1 port=5433 dbname=uqa user=uqa sslmode=disable'
```

`--database PATH` selects the persistent UQA database file and is required. The SQL database name presented to clients is `uqa`. `--listen ADDRESS` changes the default listener address, `127.0.0.1:5433`.

`--trust` explicitly selects trust authentication: the server does not verify a password. A requested role must exist, have `LOGIN`, and have `CONNECT` on database `uqa`; the new connection executes with that role's privileges. The default engine role is `uqa`. Starting without an authentication policy fails. SSL and GSS encryption requests receive a negative negotiation response.

## Query and session behavior

A connection owns an independent engine session, including its search path, configuration, transaction state, prepared SQL statements, and notification registrations. A Simple Query message can contain multiple SQL statements. Statements produce ordered row descriptions, text rows, PostgreSQL command tags, notices, and errors, followed by the session's actual transaction status. Empty messages and zero-column queries retain their distinct protocol responses.

Complete-message parsing and implicit transaction segments follow the [Simple Query engine contract](02-rust-engine-api.md#simple-query-messages). Disconnecting rolls back an open transaction. A cancellation request must present that connection's process identifier and secret; it cancels active work without authorizing queries. `LISTEN` and `NOTIFY` messages are delivered on the owning connection.

Structured SQL diagnostics preserve separate primary-message, detail, and hint protocol fields. Rewrite-rule completion reports the original command count when it survives; otherwise only a same-kind unconditional INSTEAD action can supply that count.

Result descriptions include PostgreSQL type OIDs, lengths, and modifiers. Scalar domains use their base type in the wire descriptor; domain arrays retain the array type identity. Text output follows the result's declared type, including Boolean output, character padding, temporal precision, interval field restrictions, arrays, and catalog aliases such as `regtype`.

The listener negotiates protocol versions 3.0 and 3.2. Extended Query execution, COPY streaming, binary result formats, source-table and source-column identities in row descriptions, password authentication, TLS, and multiple SQL databases remain unfinished. Extended Query messages currently report `0A000` and are discarded until `Sync`; clients must use Simple Query messages for this endpoint.

## Embed a listener

```rust
use std::sync::Arc;
use uqa_engine::Engine;
use uqa_pg_server::{Server, ServerConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Arc::new(Engine::new());
    let mut server = Server::start(
        ServerConfig {
            listen: "127.0.0.1:5433".parse()?,
            trust_authentication: true,
            ..ServerConfig::default()
        },
        engine,
    )?;
    println!("{}", server.local_addr());
    server.shutdown()?;
    Ok(())
}
```

`ServerConfig::max_message_bytes` defaults to 16 MiB and limits frontend messages. `max_protocol_version` limits negotiated protocol versions. `Server::shutdown`, or dropping the server, closes its connections, cancels active work, and joins its workers.
