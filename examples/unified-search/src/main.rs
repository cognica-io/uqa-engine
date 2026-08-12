//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The case for one engine: a paper-search pipeline that uses full-text
//! ranking, vector similarity, relational predicates, a user-defined function,
//! and a citation graph -- against one dataset, in one session, with one set of
//! row identities.
//!
//! Nothing here maps identifiers between systems, copies result sets across
//! process boundaries, or reconciles two notions of "document 4", because
//! every stage addresses the same rows.
//!
//! Run with: cargo run -p example-unified-search

use uqa_core::Value;
use uqa_engine::{Engine, SQLFunctionOptions, SQLFunctionVolatility, SQLResult};
use uqa_sql::SQLError;

const GRAPH: &str = "citations";
const CURRENT_YEAR: i64 = 2026;

/// id, title, abstract, venue, year, embedding.
/// Dimension 0 tracks "retrieval", dimension 1 "storage", dimension 2 "graph".
type Paper = (i64, &'static str, &'static str, &'static str, i64, [f64; 3]);

const PAPERS: &[Paper] = &[
    (
        1,
        "Learned sparse retrieval at scale",
        "sparse retrieval with learned term weights and inverted index pruning for ranking",
        "SIGIR",
        2024,
        [0.95, 0.10, 0.05],
    ),
    (
        2,
        "Block-max pruning revisited",
        "dynamic pruning for inverted index retrieval with block max bounds and ranking",
        "SIGIR",
        2019,
        [0.90, 0.15, 0.00],
    ),
    (
        3,
        "Vector quantization for dense retrieval",
        "dense retrieval with product quantization compressing embeddings for ranking",
        "NeurIPS",
        2025,
        [0.80, 0.35, 0.05],
    ),
    (
        4,
        "LSM trees under write amplification",
        "storage engines log structured merge trees and write amplification tradeoffs",
        "VLDB",
        2023,
        [0.05, 0.95, 0.10],
    ),
    (
        5,
        "Graph pattern matching in RDBMS",
        "graph pattern matching compiled into relational plans with worst case optimal joins",
        "VLDB",
        2025,
        [0.10, 0.25, 0.95],
    ),
    (
        6,
        "Regular path queries on property graphs",
        "regular path queries automata evaluation over property graphs and traversal",
        "ICDE",
        2018,
        [0.05, 0.10, 0.90],
    ),
];

/// Directed citation edges: (citing paper, cited paper). Every edge points
/// backwards in time, so the graph is acyclic the way a real citation graph is.
const CITES: &[(i64, i64)] = &[(3, 1), (3, 2), (1, 2), (5, 6), (5, 4)];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    load(&engine)?;
    show_lexical(&engine)?;
    show_vector_and_fusion(&engine)?;
    show_typed_operator_joins(&engine)?;
    let seed = show_udf_ranking(&engine)?;
    show_graph_composition(&engine, seed)?;
    Ok(())
}

/// Raw BM25 against the calibrated Bayesian posterior.
fn show_lexical(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== 1. Lexical retrieval: raw BM25 versus Bayesian BM25 ===");
    println!("`text_match` scores with raw BM25. `bayesian_match` returns a calibrated posterior probability instead of an unbounded score, so its values are comparable across queries and fusable by the rules in section 3.\n");
    let lexical = engine.sql(
        "SELECT id, title, year, _score \
           FROM papers \
          WHERE text_match(abstract, 'retrieval ranking') \
            AND year >= 2020 \
          ORDER BY _score DESC \
          LIMIT 5",
        &[],
    )?;
    println!("raw BM25 (text_match):");
    print_rows(&lexical, &["id", "title", "year", "_score"]);

    let bayesian = engine.sql(
        "SELECT id, title, year, _score \
           FROM papers \
          WHERE bayesian_match(abstract, 'retrieval ranking') \
            AND year >= 2020 \
          ORDER BY _score DESC \
          LIMIT 5",
        &[],
    )?;
    println!("\ncalibrated posterior (bayesian_match):");
    print_rows(&bayesian, &["id", "title", "year", "_score"]);
    Ok(())
}

/// Dense retrieval, then automatic exact fusion and an explicit robust pool.
fn show_vector_and_fusion(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 2. Vector neighbours of the same query intent ===");
    println!("A dense query vector leaning on the retrieval dimension.\n");
    let dense = engine.sql(
        "SELECT id, title, venue \
           FROM papers \
          WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 3)",
        &[],
    )?;
    print_rows(&dense, &["id", "title", "venue"]);

    println!("\n=== 3. Automatic exact fusion and explicit robust pooling ===");
    println!("A same-relation text-and-vector conjunction automatically uses exact signed likelihood-ratio addition with one prior. The explicit aliases `fuse_bayesian_evidence` and `fuse_log_odds` select that same contract. The heuristic contract has its own name: `pool_positive_evidence`.\n");
    let fused_exact = engine.sql(
        "SELECT id, title, _score \
           FROM papers \
          WHERE text_match(abstract, 'retrieval ranking') \
            AND knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4) \
          ORDER BY _score DESC \
          LIMIT 4",
        &[],
    )?;
    println!("automatic exact single-prior log-odds fusion:");
    print_rows(&fused_exact, &["id", "title", "_score"]);

    let fused_pooled = engine.sql(
        "SELECT id, title, _score \
           FROM papers \
          WHERE pool_positive_evidence( \
                    bayesian_match(abstract, 'retrieval ranking'), \
                    knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4) \
                ) \
          ORDER BY _score DESC \
          LIMIT 4",
        &[],
    )?;
    println!("\nrobust positive-evidence pooling (heuristic):");
    print_rows(&fused_pooled, &["id", "title", "_score"]);
    Ok(())
}

/// Materialize generalized identifier pairs from typed operator joins, then
/// feed one of those tuple sources into an ordinary SQL equijoin.
fn show_typed_operator_joins(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 4. Typed operator joins produce real SQL tuple sources ===");
    println!("VectorSimilarityJoin emits (left_doc_id, right_doc_id) pairs. CrossParadigmJoin bridges graph vertex properties to document fields, and DPccp can place that typed source inside a larger SQL join.\n");
    let vector_pairs = engine.sql(
        "SELECT pairs.left_doc_id AS left_id, \
                pairs.right_doc_id AS right_id, pairs._score AS score \
           FROM vector_similarity_join( \
                    papers, \
                    knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4), \
                    knn_match(embedding, ARRAY[1.0, 0.0, 0.0], 4), \
                    0.80 \
                ) AS pairs \
          ORDER BY score DESC, left_id, right_id \
          LIMIT 8",
        &[],
    )?;
    println!("vector similarity pairs:");
    print_rows(&vector_pairs, &["left_id", "right_id", "score"]);

    let graph_document_pairs = engine.sql(
        &format!(
            "SELECT pairs.left_doc_id AS vertex_id, \
                    pairs.right_doc_id AS paper_id, p.title \
               FROM cross_paradigm_join( \
                        papers, \
                        graph_pagerank('{GRAPH}'), \
                        venue IS NOT NULL \
                    ) AS pairs \
               JOIN papers AS p ON p.id = pairs.right_doc_id \
              ORDER BY vertex_id, paper_id"
        ),
        &[],
    )?;
    println!("\ngraph-to-document pairs joined back to SQL rows:");
    print_rows(&graph_document_pairs, &["vertex_id", "paper_id", "title"]);
    Ok(())
}

/// Blend BM25 with a user-defined recency boost, and return the winning id so
/// the graph stage can be seeded by an ordinary SQL row.
fn show_udf_ranking(engine: &Engine) -> Result<i64, Box<dyn std::error::Error>> {
    println!("\n=== 5. A user-defined function joins the ranking ===");
    println!("`recency_boost` decays with age; the optimizer may fold it because it is registered immutable. Final order blends BM25 with the boost.\n");
    let blended = engine.sql(
        "SELECT id, title, year, _score, \
                _score * recency_boost(year) AS blended \
           FROM papers \
          WHERE text_match(abstract, 'retrieval ranking') \
          ORDER BY blended DESC \
          LIMIT 5",
        &[],
    )?;
    print_rows(&blended, &["id", "title", "year", "_score", "blended"]);

    // The top row of that blended ranking is an ordinary SQL row, so its `id`
    // is directly usable as a graph key. No identifier translation step.
    let seed = blended
        .rows
        .first()
        .and_then(|row| row.get("id"))
        .and_then(|value| match value {
            Value::Int(id) => Some(*id),
            _ => None,
        })
        .ok_or("blended ranking returned no usable id")?;
    Ok(seed)
}

/// Graph traversal joined to the table, nested as a predicate, and finally
/// every mechanism combined in a single statement.
fn show_graph_composition(engine: &Engine, seed: i64) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== 6. Graph traversal, joined to the table in one statement ===");
    println!("Paper {seed} ranked first, so walk what it cites. `cypher(...)` is a table function, so the traversal is a relation: declaring its column `int` lets it join the integer primary key with no cast. That typed definition list is a UQA-RS extension; Apache AGE requires agtype.\n");
    let cited = engine.sql(
        &format!(
            "SELECT p.id, p.title, p.venue, p.year, recency_boost(p.year) AS boost \
               FROM papers AS p \
               JOIN cypher('{GRAPH}', $$
                        MATCH (:Paper {{paper_id: {seed}}})-[:CITES]->(cited:Paper)
                        RETURN cited.paper_id
                    $$) AS cited(id int) \
                 ON p.id = cited.id \
              WHERE p.venue <> 'ICDE' \
              ORDER BY p.year DESC"
        ),
        &[],
    )?;
    print_rows(&cited, &["id", "title", "venue", "year", "boost"]);

    println!("\n=== 7. Two-hop closure as a subquery predicate ===");
    println!("The same traversal nests inside an ordinary WHERE clause.\n");
    let reachable = engine.sql(
        &format!(
            "SELECT id, title, year FROM papers \
              WHERE id IN ( \
                    SELECT id FROM cypher('{GRAPH}', $$
                        MATCH (:Paper {{paper_id: {seed}}})-[:CITES]->()-[:CITES]->(r:Paper)
                        RETURN r.paper_id
                    $$) AS reached(id int) \
              ) \
              ORDER BY year DESC"
        ),
        &[],
    )?;
    print_rows(&reachable, &["id", "title", "year"]);

    println!("\n=== 8. Everything in one statement ===");
    println!("Full-text predicate, relational filter, user-defined function in both the projection and the ORDER BY, and a citation-graph membership test -- one plan, one pass, one set of row identities.\n");
    let unified = engine.sql(
        &format!(
            "SELECT id, title, year, \
                    _score * recency_boost(year) AS blended \
               FROM papers \
              WHERE text_match(abstract, 'retrieval ranking graph') \
                AND year >= 2018 \
                AND id IN ( \
                      SELECT id FROM cypher('{GRAPH}', $$
                          MATCH (:Paper {{paper_id: {seed}}})-[:CITES*1..2]->(r:Paper)
                          RETURN r.paper_id
                      $$) AS reached(id int) \
                ) \
              ORDER BY blended DESC \
              LIMIT 5"
        ),
        &[],
    )?;
    print_rows(&unified, &["id", "title", "year", "blended"]);
    Ok(())
}

/// Create the table, the text and vector indexes, the citation graph, and the
/// user-defined function. One session owns all four.
fn load(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    engine.sql(
        "CREATE TABLE papers (
             id INTEGER PRIMARY KEY,
             title TEXT,
             abstract TEXT,
             venue TEXT,
             year INTEGER,
             embedding VECTOR(3)
         )",
        &[],
    )?;
    engine.sql(
        "CREATE INDEX papers_abstract_gin ON papers USING gin (abstract)",
        &[],
    )?;

    for (id, title, abstract_text, venue, year, embedding) in PAPERS {
        engine.sql(
            &format!(
                "INSERT INTO papers (id, title, abstract, venue, year, embedding) \
                 VALUES ({id}, '{title}', '{abstract_text}', '{venue}', {year}, \
                         ARRAY[{}, {}, {}])",
                embedding[0], embedding[1], embedding[2]
            ),
            &[],
        )?;
    }
    engine.sql(
        "CREATE INDEX papers_embedding_hnsw ON papers USING hnsw (embedding)",
        &[],
    )?;

    // The citation graph is built through the same SQL boundary as everything
    // else; `cypher(...)` is a table function, not a separate API.
    engine.sql(&format!("SELECT create_graph('{GRAPH}') AS ok"), &[])?;
    for (id, _, _, venue, _, _) in PAPERS {
        cypher(
            engine,
            &format!("CREATE (n:Paper {{paper_id: {id}, venue: '{venue}'}})"),
        )?;
    }
    for (citing, cited) in CITES {
        cypher(
            engine,
            &format!(
                "MATCH (a:Paper {{paper_id: {citing}}}), (b:Paper {{paper_id: {cited}}}) \
                 CREATE (a)-[:CITES]->(b)"
            ),
        )?;
    }

    // Registered read-only and immutable: for a fixed argument the result never
    // changes, so the optimizer may hoist, fold, and reorder calls to it. The
    // default is the conservative opposite (volatile, may mutate engine state),
    // and the engine rejects claiming immutability while reserving mutation.
    engine.register_scalar_function_with_options(
        "recency_boost",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
        |args: &[Value]| {
            let year = match args.first() {
                Some(Value::Int(year)) => *year,
                Some(Value::Null) | None => return Ok(Value::Float(1.0)),
                other => {
                    return Err(SQLError::TypeMismatch(format!(
                        "recency_boost expects an integer year, got {other:?}"
                    )))
                }
            };
            let age = (CURRENT_YEAR - year).max(0) as f64;
            // Half-life of eight years, floored so old work is damped, not erased.
            Ok(Value::Float(0.5_f64.powf(age / 8.0).max(0.25)))
        },
    )?;

    Ok(())
}

/// Run one Cypher statement. This is an ordinary `engine.sql` call: `cypher(...)`
/// is a table function, so a graph mutation is a SQL statement like any other.
/// The helper only avoids repeating the wrapper for each of the eleven
/// statements that build the graph.
fn cypher(engine: &Engine, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    engine.sql(
        &format!("SELECT * FROM cypher('{GRAPH}', $$ {query} $$) AS (ignored agtype)"),
        &[],
    )?;
    Ok(())
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

fn render(value: Option<&Value>) -> String {
    match value {
        Some(Value::Str(text)) => text.clone(),
        Some(Value::Int(number)) => number.to_string(),
        Some(Value::Float(number)) => format!("{number:.4}"),
        Some(other) => format!("{other:?}"),
        None => "<missing>".to_string(),
    }
}
