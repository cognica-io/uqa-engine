# Graph SQL and Cypher

Named property graphs are durable engine objects. SQL reaches them through an AGE-shaped `cypher` table function, regular path queries, graph support predicates, and centrality functions.

## Graph lifecycle

| Contract part | Definition |
| --- | --- |
| Syntax | `create_graph(name TEXT)` or `drop_graph(name TEXT [, cascade BOOLEAN])` as scalar lifecycle calls, optionally qualified as `ag_catalog.create_graph` and `ag_catalog.drop_graph` |
| Arguments | `name` is a string value naming a graph, not an SQL relation identifier; a valid AGE graph name is 3 to 63 bytes, starts with a Unicode letter or underscore, continues with letters, digits, marks, underscores, dots, or dashes, and ends with a letter, digit, mark, or underscore; dropping an existing graph requires `cascade = true` |
| Result | SQL `NULL` (the AGE-compatible `void` result) on success |
| Effects | Creates or removes the durable named graph in the current transaction; a created graph reserves a namespace of the same name and owns the AGE default labels `_ag_label_vertex` (label id 1) and `_ag_label_edge` (label id 2) |
| Errors | Apache AGE messages and SQLSTATEs: a SQL `NULL` name is `graph name can not be NULL` (`22023`), an invalid name is `graph name is invalid` (`22023`), a duplicate graph is `graph "name" already exists` (`3F000`), a name that is already a schema is `schema "name" already exists` (`42P06`), a missing drop target is `graph "name" does not exist` (`3F000`), and a false or omitted drop cascade is `cannot drop schema name because other objects depend on it` (`2BP01`); wrong arity or argument types and persistence failures are also rejected |

```sql execute
SELECT create_graph('network') AS created;
SELECT drop_graph('network', true) AS dropped;
```

Aliases `graph_create` and `graph_drop` expose the native Boolean lifecycle result and are less strict than the AGE-compatible names. The Rust engine API also provides `create_graph`, `drop_graph`, and `list_graphs`.

## AGE session bootstrap

| Contract part | Definition |
| --- | --- |
| Syntax | `LOAD 'age'` followed by `SET search_path = ag_catalog, "$user", public` |
| Arguments | `LOAD` takes the shared-library string PostgreSQL clients send; `age`, `age.so`, `$libdir/age`, and `$libdir/age.so` name the embedded Apache AGE surface |
| Result | No rows |
| Effects | Loading AGE is a no-op because the engine embeds the AGE surface; the `search_path` assignment makes the bare `ag_graph` and `ag_label` names and the unqualified AGE functions resolve as they do in PostgreSQL |
| Errors | Any other library fails as `could not access file "$libdir/name": No such file or directory` (`58P01`, using the given path when it contains a directory separator) |

```sql execute
LOAD 'age';
SET search_path = ag_catalog, "$user", public;
SELECT typelem FROM pg_type WHERE typname = '_agtype';
```

Drivers such as apache-age-python run exactly this sequence and then register the `agtype` type by its `pg_type` OID; both queries succeed against the embedded catalog.

## Graph existence and label management

| Contract part | Definition |
| --- | --- |
| Syntax | `graph_exists(graph_name TEXT)`, `create_vlabel(graph_name TEXT, label_name TEXT)`, `create_elabel(graph_name TEXT, label_name TEXT)`, `drop_label(graph_name TEXT, label_name TEXT [, force BOOLEAN])`, and `alter_graph(graph_name TEXT, operation TEXT, new_value TEXT)`, optionally qualified with `ag_catalog.` |
| Arguments | Graph and label names are string values; a valid AGE label name is 1 to 63 bytes, starts with a Unicode letter or underscore, and continues with letters, digits, marks, or underscores; `operation` is the case-insensitive `RENAME`; `force` defaults to `false` |
| Result | `graph_exists` returns the agtype boolean text `true` or `false`; the other functions return SQL `NULL` (`void`) on success |
| Effects | `create_vlabel` / `create_elabel` register an empty vertex or edge label with the next label id, so its graphids are allocated ahead of any entity; `drop_label` removes a user label and every vertex (with its incident edges) or edge that carries it, like AGE's `DROP TABLE` on the label relation; `alter_graph ... RENAME` renames the graph, its namespace, and its catalog rows while keeping every entity id |
| Errors | Apache AGE messages and SQLSTATEs: `graph name must not be NULL` / `label name must not be NULL` (`22023`), `graph name is invalid` / `label name is invalid` (`22023`), `graph "name" does not exist.` (`3F000`, with the trailing period AGE prints from `create_vlabel` and `create_elabel`), `label "name" already exists` (`3F000`), `graph "name" does not exist` and `label "name" does not exist` for `drop_label` (`3F000` and `42P01`), `force option is not supported yet` (`0A000`), `cannot drop table graph.label because other objects depend on it` (`2BP01`) for the default labels (always, even when the graph is empty, unlike AGE's `DROP TABLE ... RESTRICT`), `graph_name must not be NULL` / `operation must not be NULL` / `new_value must not be NULL` (`22023`), `invalid operation "name"` (`22023`), `new graph name is invalid` (`22023`), and `schema "name" already exists` (`42P06`) when the new name is taken; `graph_exists(NULL)` is `graph name can not be NULL` (`22023`) |

```sql execute
SELECT create_graph('catalog_demo') AS created;
SELECT graph_exists('catalog_demo') AS present, graph_exists('missing') AS absent;
SELECT create_vlabel('catalog_demo', 'Person') AS vlabel;
SELECT create_elabel('catalog_demo', 'KNOWS') AS elabel;
SELECT * FROM cypher('catalog_demo', $$
    CREATE (:Person {name: 'ada'})-[:KNOWS]->(:Person {name: 'bob'}), (:City {name: 'Seoul'})
$$) AS (ignored agtype);
SELECT drop_label('catalog_demo', 'City') AS dropped_label;
SELECT alter_graph('catalog_demo', 'RENAME', 'catalog_demo_renamed') AS renamed;
```

Cypher `CREATE` and `MERGE` register labels implicitly with the same allocator, and a label keeps one kind: creating a vertex with an edge label fails as `label name is for edges, not vertices` and the reverse as `label name is for vertices, not edges` (`0A000`). The reserved names `_ag_label_vertex` and `_ag_label_edge` always denote the default labels, so Cypher entities created under them take the reserved label ids 1 and 2 instead of a new user label.

## AGE catalog relations

| Contract part | Definition |
| --- | --- |
| Syntax | `ag_catalog.ag_graph` and `ag_catalog.ag_label` in `FROM`; the bare names `ag_graph` and `ag_label` resolve while `ag_catalog` is on the session `search_path` |
| Arguments | Ordinary relations; filter, join, and project them like any table |
| Result | `ag_graph (graphid oid, name name, namespace regnamespace)` has one row per graph and `ag_label (name name, graph oid, id label_id, kind label_kind, relation regclass, seq_name name)` has one row per label, the two default labels first and then user labels by ascending label id; `kind` is `v` or `e`, `relation` is the quoted `graph.label` relation name, and `seq_name` is the label's own `<label>_id_seq` sequence (the graph-level `_label_id_seq` allocator appears only in `pg_sequences`) |
| Effects | Read-only synthesized rows that reflect the current transaction's graph catalog |
| Errors | The bare names fail as unknown relations when `ag_catalog` is not on the `search_path`, exactly like the extension schema in PostgreSQL |

```sql execute
SELECT name FROM ag_catalog.ag_graph ORDER BY name;
SELECT l.name, l.id, l.kind, l.relation, l.seq_name
FROM ag_catalog.ag_label AS l
JOIN ag_catalog.ag_graph AS g ON g.graphid = l.graph
WHERE g.name = 'catalog_demo_renamed'
ORDER BY l.id;
SELECT count(*) AS graphs FROM ag_graph WHERE name = 'catalog_demo_renamed';
```

The PostgreSQL catalogs mirror each graph the way the extension does: `pg_namespace` and `information_schema.schemata` list `ag_catalog` and one namespace per graph, `pg_class` / `pg_attribute` / `information_schema.tables` / `information_schema.columns` describe the label relations (`id graphid, properties agtype` for vertex labels and `id graphid, start_id graphid, end_id graphid, properties agtype` for edge labels), `pg_class` and `pg_sequences` list `_label_id_seq` and every `label_id_seq` with the last allocated value, and `pg_type` carries `agtype`, `_agtype`, `graphid`, `_graphid`, and the `label_id` / `label_kind` domains in the `ag_catalog` namespace. Because a graph owns its namespace, `CREATE SCHEMA` rejects a graph name as `schema already exists`, `DROP SCHEMA name` on a graph namespace fails as `cannot drop schema name because other objects depend on it` (`2BP01`), `DROP SCHEMA name CASCADE` drops the graph, and `current_schema()` / `current_schemas(...)` report `ag_catalog` and graph namespaces when the `search_path` names them. Label relations are catalog-visible metadata; the entities themselves are read through `cypher(...)` rather than through those relation names.

```sql execute
SELECT drop_graph('catalog_demo_renamed', true) AS dropped;
```

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
