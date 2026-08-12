//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

import { assertEqual } from "./common.mjs";

const GRAPH = "social";
const MEMBERS = [
  [1, "alice", 34, "seoul"],
  [2, "bob", 28, "busan"],
  [3, "carol", 41, "seoul"],
  [4, "dave", 25, "daegu"],
];

export async function runGraphCypher(engine) {
  await load(engine);
  const older = await cypher(
    engine,
    "MATCH (n:Person) WHERE n.age > 30 RETURN n.name, n.age",
    "name agtype, age agtype",
  );
  const twoHops = await cypher(
    engine,
    "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(n:Person) " +
      "RETURN DISTINCT n.name",
    "name agtype",
  );
  for (let attempt = 0; attempt < 2; attempt += 1) {
    await cypher(engine, "MERGE (:Person {member_id: 5, name: 'erin', age: 30})");
  }
  const mergeCount = await cypher(
    engine,
    "MATCH (n:Person {name: 'erin'}) RETURN count(n)",
    "copies agtype",
  );
  await cypher(engine, "MATCH (n:Person {name: 'bob'}) SET n.age = 29");
  const updated = await cypher(
    engine,
    "MATCH (n:Person {name: 'bob'}) RETURN n.age",
    "age agtype",
  );
  const joined = (
    await engine.sql(
      "SELECT m.member_id, m.name, m.city FROM members AS m " +
        "JOIN cypher('social', $$ " +
        "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->(f:Person) " +
        "RETURN f.member_id $$) AS followed(member_id int) " +
        "ON m.member_id = followed.member_id " +
        "WHERE m.city = 'seoul' ORDER BY m.member_id",
    )
  ).rows;
  const nested = (
    await engine.sql(
      "SELECT member_id, name, city FROM members WHERE member_id IN (" +
        "SELECT member_id FROM cypher('social', $$ " +
        "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(r:Person) " +
        "RETURN r.member_id $$) AS reached(member_id int)) ORDER BY member_id",
    )
  ).rows;
  assertEqual(
    joined,
    [{ member_id: 3, name: "carol", city: "seoul" }],
    "graph relational join",
  );
  assertEqual(updated, [{ age: "29" }], "graph property update");
  return { older, twoHops, mergeCount, updated, joined, nested };
}

async function load(engine) {
  await engine.sql(`SELECT create_graph('${GRAPH}') AS ok`);
  await engine.sql(
    "CREATE TABLE members (member_id INTEGER PRIMARY KEY, name TEXT, city TEXT)",
  );
  for (const [memberId, name, age, city] of MEMBERS) {
    await engine.sql(
      "INSERT INTO members (member_id, name, city) VALUES ($1, $2, $3)",
      [memberId, name, city],
    );
    await cypher(
      engine,
      `CREATE (:Person {member_id: ${memberId}, name: '${name}', age: ${age}})`,
    );
  }
  for (const [source, target] of [
    [1, 2],
    [2, 3],
    [3, 4],
    [1, 3],
  ]) {
    await cypher(
      engine,
      `MATCH (a:Person {member_id: ${source}}), (b:Person {member_id: ${target}}) ` +
        "CREATE (a)-[:FOLLOWS]->(b)",
    );
  }
}

async function cypher(engine, query, columns = "ignored agtype") {
  return (
    await engine.sql(`SELECT * FROM cypher('${GRAPH}', $$ ${query} $$) AS (${columns})`)
  ).rows;
}
