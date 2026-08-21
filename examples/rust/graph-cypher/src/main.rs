//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named graphs driven entirely from SQL through the Apache AGE-compatible
//! `cypher(...)` table function.
//!
//! Cypher is not a side door here. It is a table function, so a traversal is a
//! relation like any other: it can be filtered, joined against tables, and
//! nested in a larger statement.
//!
//! Run with: cargo run -p example-graph-cypher

use uqa_engine::{Engine, SQLResult};

const GRAPH: &str = "social";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    load(&engine)?;
    show_pattern_matching(&engine)?;
    show_mutations(&engine)?;
    show_relational_composition(&engine)?;
    Ok(())
}

/// Create the graph, the relational table that shares its identities, and the
/// follow edges between the people.
fn load(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    engine.sql(&format!("SELECT create_graph('{GRAPH}') AS ok"), &[])?;

    // A relational table that shares identities with the graph. `member_id`
    // below is the same number as `Person.member_id` in the graph.
    engine.sql(
        "CREATE TABLE members (member_id INTEGER PRIMARY KEY, name TEXT, city TEXT)",
        &[],
    )?;
    for (id, name, age, city) in [
        (1, "alice", 34, "seoul"),
        (2, "bob", 28, "busan"),
        (3, "carol", 41, "seoul"),
        (4, "dave", 25, "daegu"),
    ] {
        engine.sql(
            &format!(
                "INSERT INTO members (member_id, name, city) VALUES ({id}, '{name}', '{city}')"
            ),
            &[],
        )?;
        cypher(
            engine,
            &format!("CREATE (n:Person {{member_id: {id}, name: '{name}', age: {age}}})"),
            "()",
        )?;
    }
    for (from, to) in [(1, 2), (2, 3), (3, 4), (1, 3)] {
        cypher(
            engine,
            &format!(
                "MATCH (a:Person {{member_id: {from}}}), (b:Person {{member_id: {to}}}) \
                 CREATE (a)-[:FOLLOWS]->(b)"
            ),
            "()",
        )?;
    }
    Ok(())
}

/// Property predicates and a fixed-length traversal.
fn show_pattern_matching(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    // A pattern match with a property predicate. The definition list names the
    // output columns and their types.
    println!("People older than 30:");
    let older = cypher(
        engine,
        "MATCH (n:Person) WHERE n.age > 30 RETURN n.name, n.age",
        "(name agtype, age agtype)",
    )?;
    print_rows(&older, &["name", "age"]);

    // A fixed-length traversal. alice follows bob and carol directly, so her
    // two-hop neighbourhood is whoever those two follow.
    println!("\nTwo hops out from alice:");
    let reached = cypher(
        engine,
        "MATCH (:Person {name: 'alice'})-[:FOLLOWS]->()-[:FOLLOWS]->(reached:Person) \
         RETURN DISTINCT reached.name",
        "(name agtype)",
    )?;
    print_rows(&reached, &["name"]);
    Ok(())
}

/// MERGE idempotence and in-place SET updates.
fn show_mutations(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    // MERGE is idempotent where CREATE is not: running it twice leaves one node.
    for _ in 0..2 {
        cypher(
            engine,
            "MERGE (n:Person {member_id: 5, name: 'erin', age: 30})",
            "()",
        )?;
    }
    let copies = cypher(
        engine,
        "MATCH (n:Person {name: 'erin'}) RETURN count(n)",
        "(copies agtype)",
    )?;
    println!("\nMERGE ran twice, erin node count:");
    print_rows(&copies, &["copies"]);

    // SET updates properties in place.
    cypher(
        engine,
        "MATCH (n:Person {name: 'bob'}) SET n.age = 29",
        "()",
    )?;
    let updated = cypher(
        engine,
        "MATCH (n:Person {name: 'bob'}) RETURN n.age",
        "(age agtype)",
    )?;
    println!("\nbob's age after SET:");
    print_rows(&updated, &["age"]);
    Ok(())
}

/// The payoff: a traversal is a relation, so it joins and nests like one.
fn show_relational_composition(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    // Because a traversal is a relation, it joins against a table directly.
    // Declaring the column `int` rather than `agtype` lets it match the
    // integer primary key with no cast. That typed definition list is a
    // UQA Engine extension: Apache AGE requires `agtype` here and would need
    // `(followed.member_id::text)::int` in the join condition instead.
    println!("\nGraph traversal joined against the members table:");
    let joined = engine.sql(
        &format!(
            "SELECT m.member_id, m.name, m.city \
               FROM members AS m \
               JOIN cypher('{GRAPH}', $$
                        MATCH (:Person {{name: 'alice'}})-[:FOLLOWS]->(f:Person)
                        RETURN f.member_id
                    $$) AS followed(member_id int) \
                 ON m.member_id = followed.member_id \
              WHERE m.city = 'seoul' \
              ORDER BY m.member_id"
        ),
        &[],
    )?;
    print_rows(&joined, &["member_id", "name", "city"]);

    // The traversal also nests as a subquery, which keeps the graph condition
    // inside an otherwise ordinary relational statement.
    println!("\nSame idea as a subquery predicate:");
    let nested = engine.sql(
        &format!(
            "SELECT member_id, name, city FROM members \
              WHERE member_id IN ( \
                    SELECT member_id FROM cypher('{GRAPH}', $$
                        MATCH (:Person {{name: 'alice'}})-[:FOLLOWS]->()-[:FOLLOWS]->(r:Person)
                        RETURN r.member_id
                    $$) AS reached(member_id int) \
              ) \
              ORDER BY member_id"
        ),
        &[],
    )?;
    print_rows(&nested, &["member_id", "name", "city"]);
    Ok(())
}

/// Run one Cypher statement through the SQL table function. `columns` is the
/// definition list, for example `"(name agtype)"`, or `"()"` for a mutation
/// that returns nothing.
fn cypher(
    engine: &Engine,
    query: &str,
    columns: &str,
) -> Result<SQLResult, Box<dyn std::error::Error>> {
    let definition = if columns == "()" {
        "(ignored agtype)".to_string()
    } else {
        columns.to_string()
    };
    let result = engine.sql(
        &format!("SELECT * FROM cypher('{GRAPH}', $$ {query} $$) AS {definition}"),
        &[],
    )?;
    Ok(result)
}

fn print_rows(result: &SQLResult, columns: &[&str]) {
    if result.rows.is_empty() {
        println!("  (no rows)");
        return;
    }
    for row in &result.rows {
        let rendered = columns
            .iter()
            .map(|column| format!("{column}={}", render(row.get(*column))))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  {rendered}");
    }
}

/// `agtype` carries canonical AGE text, so strings arrive JSON-quoted while
/// numbers arrive bare. Strip the quoting for display only.
fn render(value: Option<&uqa_core::Value>) -> String {
    match value {
        Some(uqa_core::Value::Str(text)) => text.trim_matches('"').to_string(),
        Some(other) => format!("{other:?}"),
        None => "<missing>".to_string(),
    }
}
