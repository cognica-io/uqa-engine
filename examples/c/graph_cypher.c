/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"

int main(int argc, char **argv) {
    uqa_engine engine = example_open(example_arguments(argc, argv, 0));
    example_execute(engine,
        "SELECT create_graph('social');"
        "CREATE TABLE members (member_id INTEGER PRIMARY KEY, name TEXT, city TEXT);"
        "INSERT INTO members VALUES (1, 'alice', 'seoul'), (2, 'bob', 'busan'),"
        "(3, 'carol', 'seoul'), (4, 'dave', 'daegu');"
        "SELECT * FROM cypher('social', $$ CREATE "
        "(:Person {member_id: 1, name: 'alice', age: 34}),"
        "(:Person {member_id: 2, name: 'bob', age: 28}),"
        "(:Person {member_id: 3, name: 'carol', age: 41}),"
        "(:Person {member_id: 4, name: 'dave', age: 25}) $$) AS (ignored agtype)");
    const int edges[][2] = {{1, 2}, {2, 3}, {3, 4}, {1, 3}};
    char sql[512];
    for (size_t i = 0; i < sizeof(edges) / sizeof(edges[0]); ++i) {
        int length = snprintf(sql, sizeof(sql),
            "SELECT * FROM cypher('social', $$ MATCH (a:Person {member_id: %d}),"
            "(b:Person {member_id: %d}) CREATE (a)-[:FOLLOWS]->(b) $$) AS (ignored agtype)",
            edges[i][0], edges[i][1]);
        REQUIRE(length > 0 && (size_t)length < sizeof(sql));
        example_execute(engine, sql);
    }
    uqa_result result = example_query(engine,
        "SELECT * FROM cypher('social', $$ MATCH (n:Person) WHERE n.age > 30 "
        "RETURN n.name, n.age $$) AS (name agtype, age agtype) ORDER BY name");
    REQUIRE(example_rows(result) == 2);
    example_text(result, 0, 0, "\"alice\"");
    example_text(result, 1, 0, "\"carol\"");
    CHECK(uqa_result_destroy(&result));
    result = example_query(engine,
        "SELECT * FROM cypher('social', $$ MATCH (:Person {name: 'alice'})"
        "-[:FOLLOWS]->()-[:FOLLOWS]->(r:Person) RETURN DISTINCT r.name $$) "
        "AS (name agtype) ORDER BY name");
    REQUIRE(example_rows(result) == 2);
    example_text(result, 0, 0, "\"carol\"");
    example_text(result, 1, 0, "\"dave\"");
    CHECK(uqa_result_destroy(&result));
    for (int i = 0; i < 2; ++i) example_execute(engine,
        "SELECT * FROM cypher('social', $$ MERGE "
        "(:Person {member_id: 5, name: 'erin', age: 30}) $$) AS (ignored agtype)");
    REQUIRE(example_one_integer(engine,
        "SELECT * FROM cypher('social', $$ MATCH (n:Person {name: 'erin'}) "
        "RETURN count(n) $$) AS (copies int)") == 1);
    example_execute(engine,
        "SELECT * FROM cypher('social', $$ MATCH (n:Person {name: 'bob'}) "
        "SET n.age = 29 $$) AS (ignored agtype)");
    REQUIRE(example_one_integer(engine,
        "SELECT * FROM cypher('social', $$ MATCH (n:Person {name: 'bob'}) "
        "RETURN n.age $$) AS (age int)") == 29);
    result = example_query(engine,
        "SELECT m.member_id, m.name, m.city FROM members AS m JOIN "
        "cypher('social', $$ MATCH (:Person {name: 'alice'})-[:FOLLOWS]->(f:Person) "
        "RETURN f.member_id $$) AS followed(member_id int) "
        "ON m.member_id = followed.member_id WHERE m.city = 'seoul' ORDER BY m.member_id");
    const int64_t direct[] = {3};
    example_ids(result, direct, 1);
    CHECK(uqa_result_destroy(&result));
    result = example_query(engine,
        "SELECT member_id, name, city FROM members WHERE member_id IN ("
        "SELECT member_id FROM cypher('social', $$ MATCH (:Person {name: 'alice'})"
        "-[:FOLLOWS]->()-[:FOLLOWS]->(r:Person) RETURN r.member_id $$) AS reached(member_id int)) "
        "ORDER BY member_id");
    const int64_t reached[] = {3, 4};
    example_ids(result, reached, 2);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    puts("C graph and Cypher example passed");
    return EXIT_SUCCESS;
}
