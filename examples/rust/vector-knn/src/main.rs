//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector search: a `VECTOR(n)` column, the `knn_match` predicate, and the
//! three physical access paths the planner can choose between.
//!
//! Run with: cargo run -p example-vector-knn

use uqa_engine::Engine;
use uqa_engine::SQLResult;

/// Four-dimensional toy embeddings. Dimension 0 separates systems topics from
/// cooking topics, so a query vector leaning on dimension 0 should retrieve the
/// systems rows first.
const CORPUS: &[(i64, &str, &str, [f64; 4])] = &[
    (1, "async runtimes", "systems", [0.95, 0.10, 0.05, 0.00]),
    (
        2,
        "ownership and borrows",
        "systems",
        [0.90, 0.20, 0.00, 0.10],
    ),
    (3, "zero-copy parsing", "systems", [0.85, 0.05, 0.15, 0.05]),
    (4, "sourdough starters", "cooking", [0.05, 0.95, 0.10, 0.00]),
    (5, "knife skills", "cooking", [0.00, 0.90, 0.20, 0.05]),
    (
        6,
        "fermentation basics",
        "cooking",
        [0.10, 0.85, 0.05, 0.15],
    ),
];

const QUERY: [f64; 4] = [1.0, 0.0, 0.0, 0.0];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();

    engine.sql(
        "CREATE TABLE notes (
             id INTEGER PRIMARY KEY,
             title TEXT,
             topic TEXT,
             embedding VECTOR(4)
         )",
        &[],
    )?;
    for (id, title, topic, embedding) in CORPUS {
        engine.sql(
            &format!(
                "INSERT INTO notes (id, title, topic, embedding) \
                 VALUES ({id}, '{title}', '{topic}', ARRAY[{}, {}, {}, {}])",
                embedding[0], embedding[1], embedding[2], embedding[3]
            ),
            &[],
        )?;
    }

    // 1. Brute force. With no vector index the engine scans every row and
    //    computes an exact distance. This is the correctness reference the
    //    approximate paths below are compared against.
    report("brute force (exact, no index)", &knn(&engine, "notes", 3)?);

    // 2. HNSW. A graph index that trades a small amount of recall for
    //    sublinear probing. Build it after loading so the initial insert does
    //    not pay per-row graph maintenance.
    engine.sql(
        "CREATE INDEX notes_embedding_hnsw ON notes USING hnsw (embedding)",
        &[],
    )?;
    report("HNSW (approximate graph index)", &knn(&engine, "notes", 3)?);
    engine.sql("DROP INDEX notes_embedding_hnsw", &[])?;

    // 3. IVF. Partitions vectors into `lists` cells and probes the nearest
    //    `probes` of them. Probing every list is exhaustive but slow; probing
    //    one is fast and may miss neighbours that landed in another cell.
    //    `train_threshold` is lowered here only because this corpus is tiny;
    //    a real index trains on far more vectors before partitioning.
    engine.sql(
        "CREATE INDEX notes_embedding_ivf ON notes USING ivf (embedding) \
         WITH (lists = 2, probes = 2, train_threshold = 4)",
        &[],
    )?;
    report(
        "IVF (partitioned index, lists = 2, probes = 2)",
        &knn(&engine, "notes", 3)?,
    );

    // The KNN predicate is an ordinary SQL predicate, so a relational filter
    // composes with it. The filter is applied to the KNN candidate pool rather
    // than to the whole table, which is why the pool is widened to 6 here.
    let filtered = engine.sql(
        "SELECT id, title, topic \
           FROM notes \
          WHERE knn_match(embedding, ARRAY[1.0, 0.0, 0.0, 0.0], 6) \
            AND topic = 'cooking' \
          LIMIT 3",
        &[],
    )?;
    report(
        "KNN pool of 6, then filtered to topic = 'cooking'",
        &filtered,
    );

    Ok(())
}

/// Retrieve the `k` nearest rows to [`QUERY`] under whichever physical access
/// path the planner currently has available.
fn knn(engine: &Engine, table: &str, k: usize) -> Result<SQLResult, Box<dyn std::error::Error>> {
    let result = engine.sql(
        &format!(
            "SELECT id, title, topic \
               FROM {table} \
              WHERE knn_match(embedding, ARRAY[{}, {}, {}, {}], {k})",
            QUERY[0], QUERY[1], QUERY[2], QUERY[3]
        ),
        &[],
    )?;
    Ok(result)
}

fn report(label: &str, result: &SQLResult) {
    println!("\n{label}:");
    if result.rows.is_empty() {
        println!("  (no rows)");
        return;
    }
    for row in &result.rows {
        let field = |name: &str| {
            row.get(name)
                .map_or_else(|| "<missing>".to_string(), |value| format!("{value:?}"))
        };
        println!(
            "  id={} title={} topic={}",
            field("id"),
            field("title"),
            field("topic")
        );
    }
}
