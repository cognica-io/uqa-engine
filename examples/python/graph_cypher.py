#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Compose Cypher traversal with relational SQL through the Python binding."""

from __future__ import annotations

import json

import uqa


GRAPH = "social"
MEMBERS = [
    (1, "alice", 34, "seoul"),
    (2, "bob", 28, "busan"),
    (3, "carol", 41, "seoul"),
    (4, "dave", 25, "daegu"),
]


def main() -> None:
    engine = uqa.Engine()
    try:
        load(engine)
        older = cypher(
            engine,
            "MATCH (n:Person) WHERE n.age > 30 RETURN n.name, n.age",
            "name agtype, age agtype",
        )
        reached = cypher(
            engine,
            "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(n:Person) "
            "RETURN DISTINCT n.name",
            "name agtype",
        )
        for _ in range(2):
            cypher(engine, "MERGE (:Person {member_id: 5, name: 'erin', age: 30})")
        copies = cypher(
            engine,
            "MATCH (n:Person {name: 'erin'}) RETURN count(n)",
            "copies agtype",
        )
        cypher(engine, "MATCH (n:Person {name: 'bob'}) SET n.age = 29")
        updated = cypher(
            engine,
            "MATCH (n:Person {name: 'bob'}) RETURN n.age",
            "age agtype",
        )
        joined = engine.sql(
            "SELECT m.member_id, m.name, m.city FROM members AS m "
            "JOIN cypher('social', $$ "
            "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->(f:Person) "
            "RETURN f.member_id $$) AS followed(member_id int) "
            "ON m.member_id = followed.member_id "
            "WHERE m.city = 'seoul' ORDER BY m.member_id"
        ).rows
        nested = engine.sql(
            "SELECT member_id, name, city FROM members WHERE member_id IN ("
            "SELECT member_id FROM cypher('social', $$ "
            "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(r:Person) "
            "RETURN r.member_id $$) AS reached(member_id int)) ORDER BY member_id"
        ).rows
        results = {
            "older": older,
            "two_hops": reached,
            "merge_count": copies,
            "updated": updated,
            "joined": joined,
            "nested": nested,
        }
        assert joined == [{"member_id": 3, "name": "carol", "city": "seoul"}]
        assert updated == [{"age": "29"}]
        print(json.dumps(results, sort_keys=True))
    finally:
        engine.close()


def load(engine: object) -> None:
    engine.sql("SELECT create_graph('social') AS ok")
    engine.sql("CREATE TABLE members (member_id INTEGER PRIMARY KEY, name TEXT, city TEXT)")
    for member_id, name, age, city in MEMBERS:
        engine.sql(
            "INSERT INTO members (member_id, name, city) VALUES ($1, $2, $3)",
            [member_id, name, city],
        )
        cypher(
            engine,
            f"CREATE (:Person {{member_id: {member_id}, name: '{name}', age: {age}}})",
        )
    for source, target in [(1, 2), (2, 3), (3, 4), (1, 3)]:
        cypher(
            engine,
            f"MATCH (a:Person {{member_id: {source}}}), "
            f"(b:Person {{member_id: {target}}}) CREATE (a)-[:FOLLOWS]->(b)",
        )


def cypher(engine: object, query: str, columns: str = "ignored agtype") -> list:
    return engine.sql(
        f"SELECT * FROM cypher('{GRAPH}', $$ {query} $$) AS ({columns})"
    ).rows


if __name__ == "__main__":
    main()
