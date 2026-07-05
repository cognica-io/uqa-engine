//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Release-profile probes for Maek-style text/vector/fusion search paths.

use std::collections::BTreeMap;
use std::time::Instant;

use tempfile::tempdir;
use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLParam;

const LIMIT: usize = 100;

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn profile<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    println!("{label}_ms={:.3}", elapsed_ms(start));
    out
}

fn vector_for_row(row: usize) -> Vec<f32> {
    let phase = (row % 16) as f32 / 16.0;
    vec![
        1.0 - phase,
        phase,
        ((row % 7) as f32) / 7.0,
        ((row % 5) as f32) / 5.0,
        0.25,
        0.5,
        0.75,
        1.0,
    ]
}

fn row_content(id: usize) -> String {
    if id % 3 == 0 {
        format!("button search message {id} with repeated global conversation text")
    } else if id % 5 == 0 {
        format!("ancient release note {id} with searchable button text")
    } else {
        format!("ordinary conversation row {id} with global search filler")
    }
}

fn row_kind(id: usize) -> String {
    if id % 11 == 0 { "image" } else { "chat" }.to_string()
}

fn open_profile_engine(db_name: &str, create_sql: &str) -> (Engine, TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join(db_name);
    let engine = Engine::open(&db).unwrap();
    engine.sql(create_sql, &[]).unwrap();
    (engine, dir)
}

fn direct_document(id: usize) -> BTreeMap<String, Value> {
    let mut document = BTreeMap::new();
    document.insert("id".to_string(), Value::Int(id as i64));
    document.insert("content".to_string(), Value::Str(row_content(id)));
    document.insert("kind".to_string(), Value::Str(row_kind(id)));
    document.insert(
        "embedding".to_string(),
        Value::List(
            vector_for_row(id)
                .into_iter()
                .map(|value| Value::Float(f64::from(value)))
                .collect(),
        ),
    );
    document
}

fn direct_add_rows(engine: &Engine, rows: usize) {
    engine
        .transaction(|engine| {
            for id in 1..=rows {
                engine.add_document_with_vector_values(
                    "direct_messages",
                    id as u64,
                    direct_document(id),
                    BTreeMap::new(),
                );
            }
            Ok(())
        })
        .unwrap();
}

fn sql_insert_rows_without_id(engine: &Engine, table: &str, rows: usize) {
    let sql = format!("INSERT INTO {table} (content, kind, embedding) VALUES ($1, $2, $3)");
    engine
        .transaction(|engine| {
            for id in 1..=rows {
                engine.sql(
                    &sql,
                    &[
                        SQLParam::scalar(Value::Str(row_content(id))),
                        SQLParam::scalar(Value::Str(row_kind(id))),
                        SQLParam::vector(vector_for_row(id)),
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn sql_insert_rows_with_id(engine: &Engine, table: &str, rows: usize) {
    let sql = format!("INSERT INTO {table} (id, content, kind, embedding) VALUES ($1, $2, $3, $4)");
    engine
        .transaction(|engine| {
            for id in 1..=rows {
                engine.sql(
                    &sql,
                    &[
                        SQLParam::scalar(Value::Int(id as i64)),
                        SQLParam::scalar(Value::Str(row_content(id))),
                        SQLParam::scalar(Value::Str(row_kind(id))),
                        SQLParam::vector(vector_for_row(id)),
                    ],
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn assert_integer_pk_conflicts(engine: &Engine, rows: usize, expected_existing: bool) {
    let conflict_column = vec!["id".to_string()];
    let ids: Box<dyn Iterator<Item = usize>> = if expected_existing {
        Box::new(1..=rows)
    } else {
        Box::new((rows + 1)..=(rows * 2))
    };
    for id in ids {
        let value = [Value::Int(id as i64)];
        let conflict = engine.find_conflict("pk_messages", &conflict_column, &value);
        if expected_existing {
            assert_eq!(conflict, Some(id as u64));
        } else {
            assert!(conflict.is_none());
        }
    }
}

fn build_engine(rows: usize) -> (Engine, TempDir) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("search-profile.sqlite3");
    let engine = Engine::open(&db).unwrap();
    profile("create_table", || {
        engine
            .sql(
                "CREATE TABLE messages (\
                 id INTEGER PRIMARY KEY, \
                 content TEXT, \
                 kind TEXT, \
                 embedding VECTOR(8))",
                &[],
            )
            .unwrap();
    });

    profile("insert_rows", || {
        engine
            .transaction(|engine| {
                for id in 1..=rows {
                    engine.sql(
                        "INSERT INTO messages (id, content, kind, embedding) VALUES ($1, $2, $3, $4)",
                        &[
                            SQLParam::scalar(Value::Int(id as i64)),
                            SQLParam::scalar(Value::Str(row_content(id))),
                            SQLParam::scalar(Value::Str(row_kind(id))),
                            SQLParam::vector(vector_for_row(id)),
                        ],
                    )?;
                }
                Ok(())
            })
            .unwrap();
    });

    profile("create_gin_index", || {
        engine
            .sql(
                "CREATE INDEX messages_content_gin ON messages USING gin (content)",
                &[],
            )
            .unwrap();
    });
    profile("create_ivf_index", || {
        engine
            .sql(
                "CREATE INDEX messages_embedding_ivf ON messages USING ivf (embedding) \
                 WITH (lists = 16, probes = 4, train_threshold = 16)",
                &[],
            )
            .unwrap();
    });
    (engine, dir)
}

#[test]
#[ignore = "release-profile probe for persistent insert setup costs"]
fn profile_sqlite_insert_components_release() {
    let rows = std::env::var("UQA_INSERT_PROFILE_ROWS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(400);

    let (direct, _direct_dir) = open_profile_engine(
        "direct.sqlite3",
        "CREATE TABLE direct_messages (\
         id INTEGER PRIMARY KEY, \
         content TEXT, \
         kind TEXT, \
         embedding VECTOR(8))",
    );
    profile("direct_add_rows", || direct_add_rows(&direct, rows));

    let (no_pk, _no_pk_dir) = open_profile_engine(
        "no-pk.sqlite3",
        "CREATE TABLE no_pk_messages (\
         content TEXT, \
         kind TEXT, \
         embedding VECTOR(8))",
    );
    profile("sql_insert_rows_no_pk", || {
        sql_insert_rows_without_id(&no_pk, "no_pk_messages", rows);
    });

    let (id_col, _id_col_dir) = open_profile_engine(
        "id-col.sqlite3",
        "CREATE TABLE id_col_messages (\
         id INTEGER, \
         content TEXT, \
         kind TEXT, \
         embedding VECTOR(8))",
    );
    profile("sql_insert_rows_integer_column", || {
        sql_insert_rows_with_id(&id_col, "id_col_messages", rows);
    });

    let (pk, _pk_dir) = open_profile_engine(
        "pk.sqlite3",
        "CREATE TABLE pk_messages (\
         id INTEGER PRIMARY KEY, \
         content TEXT, \
         kind TEXT, \
         embedding VECTOR(8))",
    );
    profile("find_conflict_empty_integer_pk", || {
        assert_integer_pk_conflicts(&pk, rows, false);
    });
    profile("sql_insert_rows_integer_pk", || {
        sql_insert_rows_with_id(&pk, "pk_messages", rows);
    });
    profile("find_conflict_existing_integer_pk", || {
        assert_integer_pk_conflicts(&pk, rows, true);
    });
    profile("find_conflict_missing_integer_pk", || {
        assert_integer_pk_conflicts(&pk, rows, false);
    });
}

#[test]
#[ignore = "release-profile probe; run explicitly when changing search execution"]
fn profile_maek_like_global_search_sqlite_release() {
    let rows = std::env::var("UQA_SEARCH_PROFILE_ROWS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4_000);
    let (engine, _dir) = profile("setup", || build_engine(rows));
    let query = SQLParam::scalar(Value::Str("button search".to_string()));
    let embedding = SQLParam::vector(vec![1.0, 0.0, 0.2, 0.4, 0.25, 0.5, 0.75, 1.0]);

    let doc_ids = profile("table_doc_ids", || engine.table_doc_ids("messages"));
    println!("table_doc_ids_count={}", doc_ids.len());

    let text = profile("bayesian_sql", || {
        engine
            .sql(
                &format!(
                    "SELECT id, _score FROM messages \
                     WHERE bayesian_match(content, $1) \
                     ORDER BY _score DESC LIMIT {LIMIT}"
                ),
                std::slice::from_ref(&query),
            )
            .unwrap()
    });
    println!("bayesian_hits={}", text.rows.len());
    assert!(!text.rows.is_empty());

    let knn = profile("knn_sql", || {
        engine
            .sql(
                &format!(
                    "SELECT id, _score FROM messages \
                     WHERE knn_match(embedding, $1, {LIMIT}) \
                     ORDER BY _score DESC LIMIT {LIMIT}"
                ),
                std::slice::from_ref(&embedding),
            )
            .unwrap()
    });
    println!("knn_hits={}", knn.rows.len());
    assert!(!knn.rows.is_empty());

    let fused = profile("fuse_sql", || {
        engine
            .sql(
                &format!(
                    "SELECT id, kind, _score FROM messages \
                     WHERE fuse_log_odds(\
                         bayesian_match(content, $1), \
                         knn_match(embedding, $2, {LIMIT})\
                     ) AND kind = 'chat' \
                     ORDER BY _score DESC LIMIT {LIMIT}"
                ),
                &[query.clone(), embedding.clone()],
            )
            .unwrap()
    });
    println!("fuse_hits={}", fused.rows.len());
    assert!(!fused.rows.is_empty());

    let derived = profile("derived_fuse_sql", || {
        engine
            .sql(
                &format!(
                    "SELECT hits.id, hits._score FROM (\
                         SELECT id, kind, _score FROM messages \
                         WHERE fuse_log_odds(\
                             bayesian_match(content, $1), \
                             knn_match(embedding, $2, {LIMIT})\
                         ) AND kind = 'chat' \
                         ORDER BY _score DESC LIMIT {LIMIT}\
                     ) hits \
                     ORDER BY hits._score DESC LIMIT {LIMIT}"
                ),
                &[query, embedding],
            )
            .unwrap()
    });
    println!("derived_fuse_hits={}", derived.rows.len());
    assert!(!derived.rows.is_empty());
}
