# graph-cypher

```sh
cargo run -p example-graph-cypher
```

Named graphs driven entirely from SQL. Every statement here goes through `engine.sql`; `cypher('graph', $$ ... $$) AS (col type)` is an Apache AGE-compatible table function, not a separate API.

That framing is the point. Because a traversal is a relation, it can be:

- filtered and projected like any other relation,
- **joined against a table** (`JOIN cypher(...) AS followed(member_id int) ON m.member_id = followed.member_id`),
- nested as a subquery predicate inside an otherwise ordinary statement.

Declaring the definition-list column as `int` rather than `agtype` is what removes the cast: the traversal enters the relational algebra as a native integer relation instead of a payload the application has to unpack.

**This part is a UQA-RS extension, not AGE-compatible behaviour.** Apache AGE on PostgreSQL requires the column definition list to be `agtype`, and joining a traversal to an integer key there needs an explicit conversion, roughly:

```sql
-- Apache AGE: agtype only, cast at the join
JOIN cypher('social', $$ ... RETURN f.member_id $$) AS followed(member_id agtype)
  ON m.member_id = (followed.member_id::text)::int
```

The `agtype` form in this example is portable to AGE; the `int` form is not. The repository's AGE-parity fixtures under [`crates/uqa-engine/tests/`](../../crates/uqa-engine/tests) use `agtype` exclusively for exactly that reason.

Covered along the way: property predicates, fixed-length traversal, `MERGE` idempotence versus `CREATE`, and `SET`.

`agtype` carries canonical AGE text, so strings come back JSON-quoted and numbers bare; the example strips the quoting for display only.

A direct Rust API, `Engine::run_cypher`, also exists and returns rows without going through SQL. It is useful when you are not composing with relational operators, but it cannot join against tables, so this example does not use it.
