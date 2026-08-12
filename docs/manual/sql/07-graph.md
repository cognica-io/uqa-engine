# Graph SQL and Cypher

Named property graphs are durable engine objects. SQL reaches them through an AGE-shaped `cypher` table function, regular path queries, graph support predicates, and centrality functions.

## Graph lifecycle

| Contract part | Definition |
| --- | --- |
| Syntax | `create_graph(name TEXT)` or `drop_graph(name TEXT [, cascade BOOLEAN])` as scalar lifecycle calls |
| Arguments | `name` is a string value naming a graph, not an SQL relation identifier; dropping an existing AGE-compatible graph requires `cascade = true` |
| Result | SQL `NULL` (the AGE-compatible `void` result) on success |
| Effects | Creates or removes the durable named graph in the current transaction |
| Errors | Wrong arity or type, invalid names, duplicate creation, missing drop targets, a false or omitted drop cascade, and graph persistence failures are rejected |

```sql execute
SELECT create_graph('network') AS created;
SELECT drop_graph('network', true) AS dropped;
```

Aliases `graph_create` and `graph_drop` expose the native Boolean lifecycle result and are less strict than the AGE-compatible names. The Rust engine API also provides `create_graph`, `drop_graph`, and `list_graphs`.

## Cypher table function

| Contract part | Definition |
| --- | --- |
| Syntax | `cypher(graph_name TEXT, source TEXT [, parameters]) AS alias(column type, ...)` in `FROM` |
| Arguments | Graph name and Cypher source are string values; SQL calls require an output column definition list |
| Result | One relation shaped by the declared output list; `agtype` columns contain canonical AGE-shaped text |
| Effects | Read-only clauses only read graph state; supported Cypher mutations participate in the surrounding SQL transaction |
| Errors | An unknown graph, malformed or unsupported Cypher, a missing or incompatible output definition, parameter mismatches, and runtime graph errors are rejected |

```sql
SELECT *
FROM cypher('network', $$
    MATCH (n:Service)
    WHERE n.active = true
    RETURN n.service_id, n.name
$$) AS result(service_id int, name agtype);
```

The arguments are the graph name, Cypher source, and optional parameters through the typed API path. SQL calls require an output definition list. `agtype` returns canonical AGE-shaped text; UQA-RS also allows concrete SQL types in the list for direct relational composition.

## Supported Cypher clauses

| Family | Implemented clauses and forms |
| --- | --- |
| Match | `MATCH`, `OPTIONAL MATCH`, node patterns, edge patterns, path variables, fixed and variable lengths |
| Filter | `WHERE`, property access, comparisons, Boolean conditions, `IN`, string predicates |
| Pipeline | `WITH`, `UNWIND` |
| Output | `RETURN`, `DISTINCT`, aggregation, `ORDER BY`, `SKIP`, `LIMIT` |
| Create | `CREATE`, `MERGE`, `ON CREATE SET`, `ON MATCH SET` |
| Update | `SET` |
| Delete | `DELETE`, `DETACH DELETE` |

The parser and executor implement a deliberate subset, not the complete Neo4j or Apache AGE language. Port Cypher with executable fixtures.

## Create and merge

```sql
SELECT * FROM cypher('network', $$
    CREATE (a:Service {service_id: 1, name: 'api'})
$$) AS (ignored agtype);

SELECT * FROM cypher('network', $$
    MERGE (a:Service {service_id: 1})
    ON MATCH SET a.seen = true
    ON CREATE SET a.name = 'api'
$$) AS (ignored agtype);
```

`CREATE` creates a new pattern. `MERGE` matches or creates according to the implemented pattern identity and is the correct path for idempotent loading.

## Relationships and variable length

```sql
SELECT endpoint
FROM cypher('network', $$
    MATCH (:Service {service_id: 1})-[:CALLS*1..3]->(target:Service)
    RETURN DISTINCT target.service_id
$$) AS paths(endpoint int)
ORDER BY endpoint;
```

Bound variable-length traversal with a maximum when the application does not need an unbounded closure.

## Relational composition

```sql
SELECT s.service_id, s.owner
FROM services AS s
JOIN cypher('network', $$
    MATCH (:Service {name: 'api'})-[:CALLS]->(target:Service)
    RETURN target.service_id
$$) AS called(service_id int)
    ON called.service_id = s.service_id
ORDER BY s.service_id;
```

The graph result is an ordinary relation at the SQL boundary and can participate in joins, subqueries, filters, groups, and set operations supported by the relational engine.

## Regular path queries

As a table function:

```sql
SELECT *
FROM rpq('manages/manages', 1, 'network');
```

As a support predicate over a relation whose row identity matches graph vertex identity:

```sql
SELECT id
FROM people
WHERE rpq('manages*', 1, 'network')
ORDER BY id;
```

The expression grammar is:

- Label atom: `manages`
- Group: `(manages|reviews)`
- Concatenation: `manages/reviews`
- Alternation: `manages|reviews`
- Repetition: `manages*` or `manages{1,3}`

Precedence is repetition, concatenation, then alternation. The start vertex must be non-negative. When the graph argument is omitted, the two-argument form requires one unambiguous registered graph. Syntax depth is limited to 256 and NFA or DFA state count to 16,384.

## Traversal support predicates

```sql
SELECT id
FROM people
WHERE graph_traverse('network', 1, 'manages', 2)
ORDER BY id;
```

`graph_traverse(graph, start, edge_label, depth)` includes reachable support through the requested depth. A NULL label permits all labels according to the function contract.

One-hop neighbors use direction:

```sql
SELECT id
FROM people
WHERE graph_neighbors('network', 1, 'manages', 'out')
ORDER BY id;
```

`graph_edges(graph)` produces graph edge support. `traverse_match` integrates traversal with ranked retrieval, and `temporal_traverse` adds the implemented time interval constraints.

## Centrality

Named graph functions are `graph_pagerank`, `graph_hits`, and `graph_betweenness`. Short aliases `pagerank`, `hits`, and `betweenness` can use the single registered graph where unambiguous.

```sql
SELECT id, _score
FROM people
WHERE graph_pagerank('network')
ORDER BY _score DESC, id ASC;
```

Centrality produces ranked support and `_score`, so it can be filtered or fused with other retrieval signals.

## Transactions

Graph catalog, vertices, edges, path indexes, and relational state share the engine transaction protocol. Use an explicit transaction when an application writes the same identity to a table and a graph:

```sql
BEGIN;
INSERT INTO services (service_id, owner) VALUES (10, 'platform');
SELECT * FROM cypher('network', $$
    CREATE (:Service {service_id: 10, name: 'worker'})
$$) AS (ignored agtype);
COMMIT;
```

An error before commit should lead to rollback of the complete unit.
