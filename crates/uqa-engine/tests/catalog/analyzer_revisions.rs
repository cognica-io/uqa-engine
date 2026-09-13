//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable analyzer revisions, ownership, and transactional session visibility.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use uqa_core::Value;
use uqa_engine::Engine;
use uqa_storage_redb::RedbStorage;

const KEYWORD: &str = r#"{"tokenizer":{"type":"keyword"}}"#;

#[path = "analyzer_revisions/migration.rs"]
mod migration;

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    SQLite,
    Redb,
}

impl Backend {
    fn open(self, path: &Path) -> Engine {
        match self {
            Self::Memory => Engine::new(),
            Self::SQLite => Engine::open(path).unwrap(),
            Self::Redb => {
                Engine::from_persistent_provider(Arc::new(RedbStorage::open(path).unwrap()))
                    .unwrap()
            }
        }
    }
}

fn execute(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

fn hits(engine: &Engine, table: &str, field: &str, term: &str) -> Vec<i64> {
    engine
        .sql(
            &format!("SELECT id FROM {table} WHERE text_match({field}, '{term}') ORDER BY id"),
            &[],
        )
        .unwrap()
        .rows
        .iter()
        .map(|row| match row["id"] {
            Value::Int(id) => id,
            ref other => panic!("unexpected id: {other:?}"),
        })
        .collect()
}

fn synonym_file(path: &Path, rules: &str) -> String {
    std::fs::write(path, rules).unwrap();
    serde_json::json!({
        "tokenizer": {"type": "whitespace"},
        "token_filters": [{"type": "synonym", "synonyms_path": path}],
    })
    .to_string()
}

fn fixture(engine: &Engine) {
    execute(
        engine,
        "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)",
    );
    execute(engine, "CREATE INDEX docs_fts ON docs USING gin (body)");
}

#[test]
fn independent_analyzer_revisions_survive_files_names_renames_and_reopen() {
    for backend in [Backend::Memory, Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("revisions.db");
        let index_path = directory.path().join("index.txt");
        let search_path = directory.path().join("search.txt");
        let index_config = synonym_file(&index_path, "seed, stable\n");
        let search_config = synonym_file(&search_path, "query, stable\n");
        let engine = backend.open(&database);
        engine
            .register_named_analyzer("indexed", &index_config)
            .unwrap();
        engine
            .register_named_analyzer("searched", &search_config)
            .unwrap();
        fixture(&engine);
        engine
            .set_table_field_analyzer("docs", "body", "indexed", "index")
            .unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "searched", "search")
            .unwrap();
        execute(&engine, "INSERT INTO docs VALUES (1, 'seed')");
        let old_index = engine.get_table_analyzer("docs", "body", "index").unwrap();
        let old_search = engine.get_table_analyzer("docs", "body", "search").unwrap();
        assert_ne!(old_index, old_search);
        assert_eq!(
            engine.get_table_analyzer("docs", "body", "both").unwrap(),
            None
        );
        assert_eq!(hits(&engine, "docs", "body", "query"), [1]);

        engine.register_named_analyzer("indexed", KEYWORD).unwrap();
        engine.register_named_analyzer("searched", KEYWORD).unwrap();
        std::fs::remove_file(&index_path).unwrap();
        std::fs::remove_file(&search_path).unwrap();
        assert!(engine
            .drop_named_analyzer("indexed")
            .unwrap_err()
            .contains("still assigned"));
        assert!(engine
            .drop_named_analyzer("searched")
            .unwrap_err()
            .contains("still assigned"));
        execute(&engine, "ALTER TABLE docs RENAME COLUMN body TO content");
        assert_eq!(
            hits(&engine, "docs", "content", "query"),
            [1],
            "{backend:?}"
        );
        execute(&engine, "ALTER TABLE docs RENAME TO articles");
        let reopened = if matches!(backend, Backend::Memory) {
            engine
        } else {
            drop(engine);
            backend.open(&database)
        };
        assert_eq!(
            reopened
                .get_table_analyzer("articles", "content", "index")
                .unwrap(),
            old_index
        );
        assert_eq!(
            reopened
                .get_table_analyzer("articles", "content", "search")
                .unwrap(),
            old_search
        );
        execute(&reopened, "INSERT INTO articles VALUES (2, 'seed')");
        assert_eq!(
            hits(&reopened, "articles", "content", "query"),
            [1, 2],
            "{backend:?}"
        );
        execute(&reopened, "DROP INDEX docs_fts");
        assert!(reopened.drop_named_analyzer("indexed").unwrap());
        assert!(reopened.drop_named_analyzer("searched").unwrap());
        drop(reopened);
        assert!(backend
            .open(&database)
            .list_named_analyzers()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn analyzer_bindings_rollback_with_postings_and_refresh_in_sibling_sessions() {
    for backend in [Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let engine = backend.open(&directory.path().join("transactions.db"));
        fixture(&engine);
        engine.register_named_analyzer("whole", KEYWORD).unwrap();
        execute(&engine, "INSERT INTO docs VALUES (1, 'Alpha Beta')");
        let observer = engine.new_session().unwrap();
        assert_eq!(hits(&observer, "docs", "body", "alpha"), [1]);

        engine.begin().unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "whole", "both")
            .unwrap();
        assert!(hits(&engine, "docs", "body", "alpha").is_empty());
        engine.savepoint("whole_bound").unwrap();
        engine
            .set_table_field_analyzer("docs", "body", "standard", "search")
            .unwrap();
        engine.rollback_to_savepoint("whole_bound").unwrap();
        engine.release_savepoint("whole_bound").unwrap();
        assert!(engine
            .get_table_analyzer("docs", "body", "both")
            .unwrap()
            .is_some());
        engine.rollback().unwrap();
        assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
        assert_eq!(hits(&engine, "docs", "body", "alpha"), [1], "{backend:?}");
        assert_eq!(hits(&observer, "docs", "body", "alpha"), [1]);

        engine
            .set_table_field_analyzer("docs", "body", "whole", "both")
            .unwrap();
        assert!(hits(&observer, "docs", "body", "alpha").is_empty());
        assert_eq!(
            observer.table_field_analyzer("docs", "body").unwrap(),
            Some(("whole".into(), "both".into()))
        );
        engine
            .register_named_analyzer("whole", r#"{"tokenizer":{"type":"whitespace"}}"#)
            .unwrap();
        assert!(hits(&observer, "docs", "body", "alpha").is_empty());
        assert_eq!(
            observer.get_table_analyzer("docs", "body", "both").unwrap(),
            engine.get_table_analyzer("docs", "body", "both").unwrap()
        );
    }
}

#[test]
fn gin_analyzer_owner_survives_name_replacement_and_releases_to_remaining_plain_index() {
    for backend in [Backend::Memory, Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("owner.db");
        let mut engine = backend.open(&database);
        fixture(&engine);
        engine.register_named_analyzer("whole", KEYWORD).unwrap();
        execute(&engine, "INSERT INTO docs VALUES (1, 'Alpha Beta')");
        execute(
            &engine,
            "CREATE INDEX docs_owned ON docs USING gin (body) WITH (analyzer = 'whole')",
        );
        assert!(engine
            .set_table_field_analyzer("docs", "body", "standard", "search")
            .unwrap_err()
            .contains("GIN-owned"));
        engine
            .register_named_analyzer("whole", r#"{"tokenizer":{"type":"whitespace"}}"#)
            .unwrap();
        assert!(engine
            .sql(
                "CREATE INDEX docs_competing ON docs USING gin (body) WITH (analyzer = 'whole')",
                &[]
            )
            .unwrap_err()
            .to_string()
            .contains("competes"));
        if !matches!(backend, Backend::Memory) {
            drop(engine);
            engine = backend.open(&database);
        }
        assert!(hits(&engine, "docs", "body", "alpha").is_empty());
        engine.begin().unwrap();
        execute(&engine, "DROP INDEX docs_owned");
        assert_eq!(hits(&engine, "docs", "body", "alpha"), [1]);
        assert_eq!(engine.table_field_analyzer("docs", "body").unwrap(), None);
        engine.rollback().unwrap();
        assert!(hits(&engine, "docs", "body", "alpha").is_empty());
        assert!(engine.drop_named_analyzer("whole").is_err());
        execute(&engine, "DROP INDEX docs_owned");
        assert_eq!(hits(&engine, "docs", "body", "alpha"), [1]);
        assert!(engine.drop_named_analyzer("whole").unwrap());
        engine
            .set_table_field_analyzer("docs", "body", "whitespace", "index")
            .unwrap();
        assert!(engine
            .sql(
                "CREATE INDEX docs_competing ON docs USING gin (body) WITH (analyzer = 'keyword')",
                &[]
            )
            .unwrap_err()
            .to_string()
            .contains("field assignment"));
    }
}

#[test]
fn a_named_analyzer_and_an_unnamed_table_default_both_freeze_synonym_files() {
    for backend in [Backend::SQLite, Backend::Redb] {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("default.db");
        let source = directory.path().join("default.txt");
        let config = synonym_file(&source, "seed, stable\n");
        let engine = backend.open(&database);
        engine.register_named_analyzer("frozen", &config).unwrap();
        engine
            .create_table(
                "typed",
                serde_json::from_str(&config).unwrap(),
                vec!["body".into()],
            )
            .unwrap();
        engine
            .add_document(
                "typed",
                1,
                std::collections::BTreeMap::from([("body".into(), Value::Str("seed".into()))]),
            )
            .unwrap();
        engine
            .create_table(
                "typed_owned",
                serde_json::from_str(&config).unwrap(),
                Vec::new(),
            )
            .unwrap();
        std::fs::remove_file(source).unwrap();
        engine
            .add_fts_field_with_analyzer("typed_owned", "body".into(), Some("frozen"))
            .unwrap();
        engine
            .add_document(
                "typed_owned",
                1,
                std::collections::BTreeMap::from([("body".into(), Value::Str("seed".into()))]),
            )
            .unwrap();
        drop(engine);
        let reopened = backend.open(&database);
        assert_eq!(
            reopened
                .sql(
                    "SELECT body FROM typed_owned WHERE text_match(body, 'stable')",
                    &[]
                )
                .unwrap()
                .rows
                .len(),
            1
        );
        reopened
            .add_document(
                "typed",
                2,
                std::collections::BTreeMap::from([("body".into(), Value::Str("stable".into()))]),
            )
            .unwrap();
        fixture(&reopened);
        reopened
            .set_table_field_analyzer("docs", "body", "frozen", "both")
            .unwrap();
        execute(&reopened, "INSERT INTO docs VALUES (1, 'seed')");
        assert_eq!(hits(&reopened, "docs", "body", "stable"), [1]);
        assert_eq!(
            reopened
                .sql("SELECT body FROM typed WHERE text_match(body, 'seed')", &[])
                .unwrap()
                .rows
                .len(),
            2
        );
    }
}
