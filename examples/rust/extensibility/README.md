# extensibility

```sh
cargo run -p example-extensibility
```

This scenario registers scalar, table, and aggregate Rust callbacks and executes the same data and assertions as the Python, Node.js, and Browser WASM variants.

- `normalize_label` maps one SQL value to one SQL value.
- `repeat_rows` returns named columns and zero or more rows.
- `sum_squares` creates independent state for each SQL aggregate group, observes every input row, and finishes once per group.

The callbacks use `SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable)`. `SQLFunctionOptions::default()` is deliberately conservative: volatile and permitted to mutate engine state. A declaration that permits engine mutation cannot also claim stable or immutable volatility, and registration rejects that contradictory combination.

Host callbacks belong to the engine callback registry and are not stored in a database file. Register them again after creating a new engine process; sessions created from the same engine share the registry.
