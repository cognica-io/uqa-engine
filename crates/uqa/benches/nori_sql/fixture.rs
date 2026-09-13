//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public SQL fixtures and closed-file database setup for application measurements.

use std::path::Path;

use serde_json::{json, Value as JSONValue};
use sha2::{Digest, Sha256};
use uqa::{Engine, SQLParam, SQLResult, Value};

pub const DOCUMENTS: usize = 2048;
pub const QUERY: &str = "SELECT id, _score FROM docs WHERE fts_match(body, $1) ORDER BY id";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Memory,
    SQLite,
    #[cfg(not(target_os = "emscripten"))]
    Redb,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::SQLite => "sqlite",
            #[cfg(not(target_os = "emscripten"))]
            Self::Redb => "redb",
        }
    }

    pub fn open(self, path: &Path) -> Engine {
        let engine = match self {
            Self::Memory => Engine::new(),
            Self::SQLite => Engine::open(path).unwrap(),
            #[cfg(not(target_os = "emscripten"))]
            Self::Redb => Engine::from_persistent_provider(std::sync::Arc::new(
                uqa_storage_redb::RedbStorage::open(path).unwrap(),
            ))
            .unwrap(),
        };
        configure(&engine);
        engine
    }
}

pub fn configure(engine: &Engine) {
    engine.sql("SET work_mem = '256MB'", &[]).unwrap();
}

pub struct Database {
    // Release database handles before removing the directory.
    pub engine: Engine,
    pub directory: tempfile::TempDir,
    pub provider: Provider,
}

impl Database {
    pub fn empty(provider: Provider) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let engine = provider.open(&directory.path().join("nori.db"));
        Self {
            engine,
            directory,
            provider,
        }
    }

    pub fn seed(
        provider: Provider,
        mode: &str,
        count: usize,
        controls: bool,
        cases: &[JSONValue],
    ) -> Self {
        let database = Self::empty(provider);
        let engine = &database.engine;
        engine
            .sql("CREATE TABLE docs (id BIGINT PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        let config = json!({
            "tokenizer": {"type": "nori_tokenizer", "decompound_mode": mode},
            "token_filters": [{"type": "nori_part_of_speech"}, {"type": "nori_readingform"}, {"type": "unicode_simple_lowercase"}],
        });
        engine
            .sql(
                "SELECT * FROM create_analyzer('bench_nori', $1)",
                &[SQLParam::scalar(Value::Str(config.to_string()))],
            )
            .unwrap();
        engine
            .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        engine
            .sql(
                "SELECT * FROM set_table_analyzer('docs', 'body', 'bench_nori', 'both')",
                &[],
            )
            .unwrap();
        engine.sql("BEGIN", &[]).unwrap();
        insert(engine, &inserts(0, count, cases));
        if controls {
            for (index, case) in cases.iter().enumerate() {
                engine
                    .sql(
                        "INSERT INTO docs VALUES ($1, $2)",
                        &[
                            SQLParam::scalar(Value::Int((count + index) as i64)),
                            SQLParam::scalar(Value::Str(case["text"].as_str().unwrap().into())),
                        ],
                    )
                    .unwrap();
            }
        }
        engine.sql("COMMIT", &[]).unwrap();
        engine.sql("ANALYZE docs", &[]).unwrap();
        database
    }

    pub fn close(self) -> tempfile::TempDir {
        drop(self.engine);
        self.directory
    }

    pub fn reopen(self) -> Self {
        assert!(self.provider != Provider::Memory);
        let provider = self.provider;
        let directory = self.close();
        let engine = provider.open(&directory.path().join("nori.db"));
        Self {
            engine,
            directory,
            provider,
        }
    }

    pub fn copy_seed(provider: Provider, seed: &Path) -> Self {
        assert!(provider != Provider::Memory);
        let directory = tempfile::tempdir().unwrap();
        for file in std::fs::read_dir(seed).unwrap() {
            let file = file.unwrap();
            assert!(file.file_type().unwrap().is_file());
            std::fs::copy(file.path(), directory.path().join(file.file_name())).unwrap();
        }
        let engine = provider.open(&directory.path().join("nori.db"));
        Self {
            engine,
            directory,
            provider,
        }
    }
}

pub fn inserts(start: usize, count: usize, cases: &[JSONValue]) -> Vec<(String, Vec<SQLParam>)> {
    (start..start + count)
        .collect::<Vec<_>>()
        .chunks(64)
        .map(|ids| {
            let values = (0..ids.len())
                .map(|i| format!("(${}, ${})", i * 2 + 1, i * 2 + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut params = Vec::with_capacity(ids.len() * 2);
            for id in ids {
                let case = &cases[id % cases.len()];
                params.push(SQLParam::scalar(Value::Int(*id as i64)));
                params.push(SQLParam::scalar(Value::Str(
                    case["text"]
                        .as_str()
                        .unwrap()
                        .repeat(case["repeat"].as_u64().unwrap() as usize),
                )));
            }
            (format!("INSERT INTO docs VALUES {values}"), params)
        })
        .collect()
}

pub fn insert(engine: &Engine, statements: &[(String, Vec<SQLParam>)]) -> usize {
    statements
        .iter()
        .map(|(sql, params)| engine.sql(sql, params).unwrap().affected_rows as usize)
        .sum()
}

pub fn phrase(case: &JSONValue) -> Vec<SQLParam> {
    let text = case["text"].as_str().unwrap();
    assert!(!text.contains('"'));
    vec![SQLParam::scalar(Value::Str(format!("\"{text}\"")))]
}

pub fn scored_rows(
    result: &SQLResult,
    case: usize,
    count: usize,
    controls: bool,
    cases: usize,
) -> Vec<(i64, f64)> {
    let mut expected: Vec<_> = (case..count).step_by(cases).map(|id| id as i64).collect();
    if controls {
        expected.push((count + case) as i64);
    }
    let rows: Vec<_> = result
        .rows
        .iter()
        .map(|row| {
            let Value::Int(id) = row["id"] else {
                panic!("integer document identity required")
            };
            let Value::Float(score) = row["_score"] else {
                panic!("floating SQL score required")
            };
            assert!(score.is_finite() && score > 0.0);
            (id, score)
        })
        .collect();
    assert_eq!(rows.iter().map(|&(id, _)| id).collect::<Vec<_>>(), expected);
    rows
}

pub fn same_scores(actual: &[(i64, f64)], expected: &[(i64, f64)]) {
    assert_eq!(actual.len(), expected.len());
    for (&(id, score), &(expected_id, expected_score)) in actual.iter().zip(expected) {
        assert_eq!(id, expected_id);
        assert!((score - expected_score).abs() <= 1e-12 * expected_score.abs().max(1.0));
    }
}

pub fn fingerprint(engine: &Engine) -> String {
    let result = engine.sql("SELECT analysis ->> 'analyzer_fingerprint' AS fingerprint FROM analyze_text('bench_nori', '세종시')", &[]).unwrap();
    let Value::Str(value) = &result.rows[0]["fingerprint"] else {
        panic!("analyzer fingerprint required")
    };
    value.clone()
}

pub fn snapshot(engine: &Engine, count: usize, cases: &[JSONValue]) -> JSONValue {
    let rows = engine
        .sql("SELECT id, body FROM docs ORDER BY id", &[])
        .unwrap();
    assert_eq!(rows.rows.len(), count);
    let mut hash = Sha256::new();
    for (id, row) in rows.rows.iter().enumerate() {
        let Value::Int(actual_id) = row["id"] else {
            panic!("integer id required")
        };
        let Value::Str(body) = &row["body"] else {
            panic!("original source required")
        };
        assert_eq!(actual_id, id as i64);
        let case = &cases[id % cases.len()];
        assert_eq!(
            *body,
            case["text"]
                .as_str()
                .unwrap()
                .repeat(case["repeat"].as_u64().unwrap() as usize)
        );
        hash.update(actual_id.to_le_bytes());
        hash.update((body.len() as u64).to_le_bytes());
        hash.update(body.as_bytes());
    }
    let queries: Vec<_> = cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            scored_rows(
                &engine.sql(QUERY, &phrase(case)).unwrap(),
                index,
                count,
                false,
                cases.len(),
            )
        })
        .collect();
    json!({"documents": count, "rows_sha256": format!("{:x}", hash.finalize()), "analyzer_fingerprint": fingerprint(engine), "queries": queries})
}
