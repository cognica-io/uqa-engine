# Tutorial 5: Graphs and Cypher

This tutorial creates a named social graph, executes Cypher as a SQL table function, and joins traversal results to relational member data.

## 1. Create relational identities

```sql
CREATE TABLE members (
    member_id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    city TEXT NOT NULL
);

INSERT INTO members (member_id, name, city) VALUES
    (1, 'alice', 'seoul'),
    (2, 'bob', 'busan'),
    (3, 'carol', 'seoul'),
    (4, 'dave', 'daegu');
```

The graph will store the same `member_id` property so traversal results can join to the table without an external identity map.

## 2. Create a named graph

```sql
SELECT create_graph('social') AS created;
```

List graphs in `usql` with `\dg`.

## 3. Create vertices

Run one Cypher mutation per member:

```sql
SELECT * FROM cypher('social', $$
    CREATE (n:Person {member_id: 1, name: 'alice', age: 34})
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    CREATE (n:Person {member_id: 2, name: 'bob', age: 28})
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    CREATE (n:Person {member_id: 3, name: 'carol', age: 41})
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    CREATE (n:Person {member_id: 4, name: 'dave', age: 25})
$$) AS (ignored agtype);
```

The definition list is required even when the mutation returns no value.

## 4. Create relationships

```sql
SELECT * FROM cypher('social', $$
    MATCH (a:Person {name: 'alice'}), (b:Person {name: 'bob'})
    CREATE (a)-[:FOLLOWS]->(b)
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    MATCH (a:Person {name: 'alice'}), (b:Person {name: 'carol'})
    CREATE (a)-[:FOLLOWS]->(b)
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    MATCH (a:Person {name: 'bob'}), (b:Person {name: 'carol'})
    CREATE (a)-[:FOLLOWS]->(b)
$$) AS (ignored agtype);

SELECT * FROM cypher('social', $$
    MATCH (a:Person {name: 'carol'}), (b:Person {name: 'dave'})
    CREATE (a)-[:FOLLOWS]->(b)
$$) AS (ignored agtype);
```

Use `MERGE` instead of `CREATE` when repeated execution should match an existing pattern rather than create another one.

## 5. Match properties and paths

```sql
SELECT name, age
FROM cypher('social', $$
    MATCH (n:Person)
    WHERE n.age > 30
    RETURN n.name, n.age
$$) AS (name agtype, age agtype)
ORDER BY name;
```

Traverse two relationships from Alice:

```sql
SELECT name
FROM cypher('social', $$
    MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(reached:Person)
    RETURN DISTINCT reached.name
$$) AS (name agtype)
ORDER BY name;
```

## 6. Join traversal output to SQL

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

The typed `int` definition lets the graph property join directly to the relational primary key. This typed definition is a UQA-RS extension to the AGE-shaped interface.

## 7. Mutate and verify

```sql
SELECT * FROM cypher('social', $$
    MATCH (n:Person {name: 'bob'})
    SET n.age = 29
$$) AS (ignored agtype);

SELECT age
FROM cypher('social', $$
    MATCH (n:Person {name: 'bob'})
    RETURN n.age
$$) AS (age agtype);
```

Graph mutations can share an explicit transaction with relational SQL when both representations must commit together.

## 8. Run the complete example

```sh
cargo run -p example-graph-cypher
```

The example covers `MERGE`, `SET`, traversal, joins, and subqueries. Continue with the [graph reference](../reference/07-graphs.md) for regular path queries, analytics, and typed APIs.
