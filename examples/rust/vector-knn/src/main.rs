//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact, HNSW, IVF and `DiskANN` scores, transactional changes and persistent reopen.
//!
//! Run with: cargo run -p example-vector-knn

use std::path::Path;
use uqa_core::Value;
use uqa_engine::{Engine, SQLParam, SQLResult};

const CORPUS: &[(i64, &str, &str, [f32; 4])] = &[
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    scenario(&Engine::new())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("uqa-rust-vector-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = persistent(&directory.join("vectors.db"));
    let cleanup = std::fs::remove_dir_all(&directory);
    result?;
    cleanup?;
    Ok(())
}

fn persistent(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let expected = {
        let engine = Engine::open(path)?;
        scenario(&engine)?
    };
    let reopened = Engine::open(path)?;
    let rows = knn(&reopened, 6)?;
    assert_eq!(rows.rows, expected.rows, "reopened DiskANN scores and rows");
    let method = reopened.sql(
        "SELECT indexdef FROM pg_indexes WHERE indexname='notes_embedding_diskann'",
        &[],
    )?;
    let Value::Str(definition) = &method.rows[0]["indexdef"] else {
        panic!("missing reopened index definition");
    };
    assert!(definition.to_ascii_lowercase().contains("using diskann"));
    report("reopened DiskANN", &rows);
    Ok(())
}

fn scenario(engine: &Engine) -> Result<SQLResult, Box<dyn std::error::Error>> {
    engine.sql(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, title TEXT, topic TEXT, embedding VECTOR(4))",
        &[],
    )?;
    for (id, title, topic, embedding) in CORPUS {
        engine.sql(
            "INSERT INTO notes (id,title,topic,embedding) VALUES ($1,$2,$3,$4)",
            &[
                SQLParam::scalar(Value::Int(*id)),
                SQLParam::scalar(Value::Str((*title).into())),
                SQLParam::scalar(Value::Str((*topic).into())),
                SQLParam::vector(embedding.to_vec()),
            ],
        )?;
    }
    let exact = knn(engine, 3)?;
    report("exact", &exact);
    for (method, options) in [
        ("hnsw", ""),
        ("ivf", " WITH (lists=2, probes=2, train_threshold=4)"),
        (
            "diskann",
            " WITH (max_degree=4, search_list_size=16, beam_width=2)",
        ),
    ] {
        engine.sql(
            &format!(
                "CREATE INDEX notes_embedding_{method} ON notes USING {method}(embedding){options}"
            ),
            &[],
        )?;
        let rows = knn(engine, 3)?;
        assert_eq!(rows.rows[0]["id"], Value::Int(1));
        report(method, &rows);
        if method == "diskann" {
            assert_eq!(rows.rows, exact.rows, "DiskANN canonical scores and rows");
            assert_eq!(
                rows.rows
                    .iter()
                    .map(|row| row["id"].clone())
                    .collect::<Vec<_>>(),
                [Value::Int(1), Value::Int(3), Value::Int(2)]
            );
        } else {
            engine.sql(&format!("DROP INDEX notes_embedding_{method}"), &[])?;
        }
    }
    let filtered = engine.sql("SELECT id,title,topic FROM notes WHERE knn_match(embedding,ARRAY[1.0,0.0,0.0,0.0],6) AND topic='cooking' ORDER BY _score DESC,id LIMIT 3", &[])?;
    assert_eq!(filtered.rows.len(), 3);
    assert!(filtered
        .rows
        .iter()
        .all(|row| row["topic"] == Value::Str("cooking".into())));
    report("filtered DiskANN", &filtered);

    engine.sql("BEGIN", &[])?;
    replace(engine)?;
    let private = knn(engine, 3)?;
    assert_eq!(private.rows[0]["id"], Value::Int(6));
    assert_eq!(private.rows[0]["_score"], Value::Float(1.0));
    engine.sql("ROLLBACK", &[])?;
    assert_eq!(knn(engine, 3)?.rows, exact.rows, "DiskANN rollback");
    replace(engine)?;
    let committed = knn(engine, 6)?;
    assert_eq!(
        committed
            .rows
            .iter()
            .map(|row| row["id"].clone())
            .collect::<Vec<_>>(),
        [6, 1, 3, 2, 4, 5].map(Value::Int),
        "complete committed candidate pool"
    );
    assert_eq!(committed.rows[0]["id"], Value::Int(6));
    assert_eq!(committed.rows[0]["_score"], Value::Float(1.0));
    Ok(committed)
}

fn replace(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    engine.sql(
        "UPDATE notes SET embedding=$1 WHERE id=6",
        &[SQLParam::vector(vec![1.0, 0.0, 0.0, 0.0])],
    )?;
    Ok(())
}

fn knn(engine: &Engine, k: i64) -> Result<SQLResult, Box<dyn std::error::Error>> {
    Ok(engine.sql(
        "SELECT id,title,topic,_score FROM notes WHERE knn_match(embedding,$1,$2) ORDER BY _score DESC,id",
        &[SQLParam::vector(vec![1.0, 0.0, 0.0, 0.0]), SQLParam::scalar(Value::Int(k))],
    )?)
}

fn report(label: &str, result: &SQLResult) {
    println!("{label}: {:?}", result.rows);
}
