# extensibility

```sh
cargo run -p example-extensibility
```

Two ways to add behaviour to the engine, and how they compose.

**Rust callbacks**, registered on the session:

- A scalar function is any `Fn(&[Value]) -> Result<Value, SQLError>`.
- An aggregate is a factory for per-group state implementing `SQLAggregateState`: `observe` per row, `finish` per group.

**PL/pgSQL routines**, stored in the catalog, so they survive reopen and are visible to every session on the database. The example defines one with `IF` / `ELSIF` branching.

The last query calls a Rust callback and a SQL routine in the same statement, including in a `WHERE` clause; neither knows the other is not built in.

## Declaring function properties

`SQLFunctionOptions::default()` is deliberately the most conservative pair: `Volatile` **and** may-mutate-engine-state. Opting into optimizer freedom means declaring both properties, which is what `SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable)` does.

The engine rejects the contradictory combination at registration time rather than miscompiling a plan later, so `..SQLFunctionOptions::default()` alongside `Immutable` is an error, not a silent downgrade.
