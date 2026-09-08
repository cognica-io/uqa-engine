# Graphs

UQA Engine stores named property graphs and composes graph results with relational SQL. Applications can use typed engine methods, Cypher through a SQL table function, regular path queries, and graph analytics functions.

## Create a named graph

From SQL:

```sql
SELECT create_graph('social') AS created;
```

From Rust:

```rust
engine.create_graph("social")?;
```

Graph names share durable catalog state. `drop_graph` removes a graph and its contents, so treat it as destructive DDL.

## Apache AGE catalog and labels

AGE clients bootstrap with `LOAD 'age'` and `SET search_path = ag_catalog, "$user", public`, probe graphs through `ag_catalog.ag_graph` or `graph_exists`, and read surviving labels from `ag_catalog.ag_label`; all of these work against the embedded engine, and `create_vlabel`, `create_elabel`, `drop_label`, and `alter_graph` manage labels and graph names with AGE's dependency, dangling-edge, and broken-default lifecycle. The exact contracts are in [Graph SQL and Cypher](../sql/07-graph.md).

```sql
LOAD 'age';
SET search_path = ag_catalog, "$user", public;
SELECT count(*) FROM ag_graph WHERE name = 'social';
SELECT create_vlabel('social', 'Person');
```

## Execute Cypher through SQL

The PostgreSQL AGE-shaped entry point is the `cypher` table function:

```sql
SELECT *
FROM cypher('social', $$
    CREATE (n:Person {member_id: 1, name: 'alice', age: 34})
$$) AS (ignored agtype);
```

Every table function call needs a column definition list. Use an ignored `agtype` column for a mutation that returns no values.

Create an edge:

```sql
SELECT *
FROM cypher('social', $$
    MATCH (a:Person {name: 'alice'}), (b:Person {name: 'bob'})
    CREATE (a)-[:FOLLOWS]->(b)
$$) AS (ignored agtype);
```

Read graph values:

```sql
SELECT name, age
FROM cypher('social', $$
    MATCH (n:Person)
    WHERE n.age > 30
    RETURN n.name, n.age
$$) AS (name agtype, age agtype)
ORDER BY name;
```

UQA Engine accepts typed definition lists such as `member_id int` when the returned property should join directly to a relational integer. This is a UQA Engine extension to the AGE-shaped interface.

## Supported Cypher surface

The implemented subset includes:

- `MATCH` and `OPTIONAL MATCH`
- Node, directed edge, fixed-length, and variable-length patterns
- Path variables
- `CREATE` and `MERGE`
- `ON CREATE SET` and `ON MATCH SET`
- `SET`, `DELETE`, and `DETACH DELETE`
- `WHERE`, `RETURN`, `WITH`, and `DISTINCT`
- Aggregation, `ORDER BY`, `SKIP`, and `LIMIT`
- `UNWIND`
- Parameters through the typed `run_cypher` API

Consult [Graph SQL and Cypher](../sql/07-graph.md) for grammar details and compatibility boundaries.

## Compose graph and relational data

A Cypher result is a relation, so it can be joined, filtered, grouped, or used inside a subquery:

```sql
SELECT m.member_id, m.name, m.city
FROM members AS m
JOIN cypher('social', $$
    MATCH (:Person {name: 'alice'})-[:FOLLOWS]->(p:Person)
    RETURN p.member_id
$$) AS followed(member_id int)
    ON followed.member_id = m.member_id
WHERE m.city = 'seoul'
ORDER BY m.member_id;
```

```mermaid
flowchart LR
    A[Named graph] --> B[Cypher table function]
    C[Relational table] --> D[SQL join]
    B --> D
    D --> E[Filtered and projected result]
```

## Typed Cypher API

`Engine::run_cypher(graph, query, params)` executes Cypher directly and returns a pair of column names and result rows: `(Vec<String>, Vec<ResultRow>)`. Bind parameter values through the provided map rather than building untrusted Cypher text.

Python exposes `run_cypher`, Node.js exposes `runCypher` and `runCypherSync`, and browser WASM exposes the corresponding asynchronous request path.

`Engine::graph_with` scopes direct reads to one physical storage snapshot and passes a `GraphStoreHandle`. `Engine::new()` uses primary memory storage; persistent engines use `PersistentGraphStore` handles without loading a graph replica. `GraphStore::get_vertex` and `get_edge` return owned `Result<Option<Vertex>>` and `Result<Option<Edge>>`; callers must handle storage errors as well as missing entities. Label, adjacency, membership, count, and lifecycle methods are fallible too. Bounded `vertex_id_page` and `edge_id_page` methods accept an exclusive ID cursor and a page size from 1 through 4,096.

```rust
use uqa_graph::GraphStore;

let vertex = engine
    .graph_with("social", |store| store.get_vertex(42))?
    .transpose()?
    .flatten();
```

`graph_with_mut` callbacks return `GraphStoreResult<T>` and run inside a storage checkpoint. Errors and panics roll back the callback's writes. A standalone `SQLiteGraphStore` also reads indexed durable records directly; use its `read_snapshot` callback for a multi-read operation. Callers sharing a physical storage session must serialize transaction ownership.

## Regular path queries

`rpq` evaluates a regular expression over edge labels. Its expression language supports:

- A label atom
- Parentheses
- Concatenation with `/`
- Alternation with `|`
- Repetition with `*` or `{min,max}`

Precedence is repetition, then concatenation, then alternation. The compiler limits syntax depth and automaton size to bound hostile or accidental query growth. The current limits are 256 AST levels and 16,384 NFA or DFA states.

## Traversal and analytics

SQL retrieval functions include graph traversal, neighbors, edge inspection, PageRank, HITS, and betweenness centrality. Graph results can also participate in scored retrieval and fusion. Function signatures are listed in [Graph SQL and Cypher](../sql/07-graph.md).

## Mutation behavior

Graph mutations participate in engine transaction state. Use an explicit SQL transaction when graph and relational changes must commit together. `MERGE` is the idempotent creation path for a matched pattern; `CREATE` always creates. `DELETE` requires normal relationship safety, while `DETACH DELETE` removes incident relationships with the node.

## Path indexes

The engine API can create, list, and drop path indexes for repeated graph path workloads. Persistent path definitions and reachability pairs live in storage; opening an engine or session binds handles without rebuilding or loading all pairs. `PathIndex::lookup` returns an owned, fallible `Result<Option<BTreeSet<(u64, u64)>>>`. Invalidated or legacy materializations evaluate only the requested sequence in the current read view, without rebuilding an in-memory index or writing during a read. Engine graph mutations invalidate dependent path-index registrations; storage-level mutations invalidate physical materializations atomically, including every graph sharing a changed entity.

## Related material

- [Graph tutorial](../tutorials/05-graphs-and-cypher.md)
- [Graph SQL reference](../sql/07-graph.md)
- [Graph runtime internals](../internals/06-graph-runtime.md)
