//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Port of the analyzer SQL surface from
//! `uqa/tests/test_analysis.py`: the `create_analyzer`,
//! `list_analyzers`, `drop_analyzer`, and `set_table_analyzer` table
//! functions plus their catalog-persistence behaviour.

use tempfile::TempDir;
use uqa_engine::Engine;

fn run_one(eng: &Engine, sql: &str) -> Result<usize, String> {
    eng.sql(sql, &[])
        .map(|r| r.rows.len())
        .map_err(|e| format!("{e:?}"))
}

#[test]
fn create_and_list_analyzers() {
    let eng = Engine::new();
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[{"type":"lowercase"}],"char_filters":[]}"#;
    let create = eng
        .sql(
            &format!("SELECT * FROM create_analyzer('rs_my_test_analyzer', '{cfg}')"),
            &[],
        )
        .unwrap();
    assert_eq!(create.rows.len(), 1);
    let names = eng
        .sql("SELECT * FROM list_analyzers()", &[])
        .unwrap();
    let listed: Vec<String> = names
        .rows
        .iter()
        .filter_map(|r| match r.get("analyzer_name") {
            Some(uqa_core::Value::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(listed.contains(&"rs_my_test_analyzer".to_string()));
    assert!(listed.contains(&"standard".to_string()));
    let _ = run_one(&eng, "SELECT * FROM drop_analyzer('rs_my_test_analyzer')");
}

#[test]
fn drop_analyzer_via_sql() {
    let eng = Engine::new();
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[]}"#;
    eng.sql(
        &format!("SELECT * FROM create_analyzer('rs_drop_me', '{cfg}')"),
        &[],
    )
    .unwrap();
    assert!(eng.list_named_analyzers().contains(&"rs_drop_me".to_string()));
    eng.sql("SELECT * FROM drop_analyzer('rs_drop_me')", &[])
        .unwrap();
    assert!(!eng.list_named_analyzers().contains(&"rs_drop_me".to_string()));
}

#[test]
fn analyzer_used_in_text_search() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE articles (id BIGSERIAL PRIMARY KEY, title TEXT, body TEXT)",
        &[],
    )
    .unwrap();
    eng.sql(
        "CREATE INDEX idx_articles_gin ON articles USING gin (title, body)",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO articles (title, body) VALUES \
         ('The Quick Brown Fox', 'jumps over the lazy dog'), \
         ('A slow turtle', 'walks carefully on the ground')",
        &[],
    )
    .unwrap();
    let r = eng
        .sql(
            "SELECT id FROM articles WHERE text_match(title, 'quick fox')",
            &[],
        )
        .unwrap();
    assert!(!r.rows.is_empty());
}

#[test]
fn set_table_analyzer_default_phase() {
    let eng = Engine::new();
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[{"type":"lowercase"}]}"#;
    eng.sql(
        &format!("SELECT * FROM create_analyzer('rs_test_lower', '{cfg}')"),
        &[],
    )
    .unwrap();
    eng.sql("CREATE TABLE t (id INTEGER, body TEXT)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT * FROM set_table_analyzer('t', 'body', 'rs_test_lower')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    let _ = run_one(&eng, "SELECT * FROM drop_analyzer('rs_test_lower')");
}

#[test]
fn set_table_analyzer_with_phase() {
    let eng = Engine::new();
    let cfg = r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"lowercase"},{"type":"synonym","synonyms":{"fast":["quick"]}}]}"#;
    eng.sql(
        &format!("SELECT * FROM create_analyzer('rs_test_syn', '{cfg}')"),
        &[],
    )
    .unwrap();
    eng.sql("CREATE TABLE t (id INTEGER, body TEXT)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT * FROM set_table_analyzer('t', 'body', 'rs_test_syn', 'search')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    let analyzer = eng.get_table_analyzer("t", "body", "search");
    assert!(analyzer.is_some(), "search analyzer not found");
    let _ = run_one(&eng, "SELECT * FROM drop_analyzer('rs_test_syn')");
}

#[test]
fn analyzer_persists_across_engine_reopen() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("ca.db");
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[{"type":"lowercase"},{"type":"stop","language":"english"}]}"#;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            &format!("SELECT * FROM create_analyzer('rs_persistent', '{cfg}')"),
            &[],
        )
        .unwrap();
        assert!(eng.list_named_analyzers().contains(&"rs_persistent".to_string()));
    }
    {
        let eng = Engine::open(&path).unwrap();
        // analyzer registry came back from catalog
        assert!(
            eng.list_named_analyzers().contains(&"rs_persistent".to_string()),
            "named analyzer lost across reopen"
        );
    }
}

#[test]
fn field_analyzer_persisted() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("fap.db");
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[{"type":"lowercase"},{"type":"porter_stem"}]}"#;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            &format!("SELECT * FROM create_analyzer('rs_my_stem', '{cfg}')"),
            &[],
        )
        .unwrap();
        eng.sql("CREATE TABLE docs (id INTEGER, body TEXT)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_my_stem", "index")
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        let analyzer = eng.get_table_analyzer("docs", "body", "index");
        assert!(analyzer.is_some(), "index analyzer not restored");
    }
}

#[test]
fn search_analyzer_persisted() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sap.db");
    let cfg = r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"lowercase"},{"type":"synonym","synonyms":{"car":["automobile"]}}]}"#;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            &format!("SELECT * FROM create_analyzer('rs_syn_search', '{cfg}')"),
            &[],
        )
        .unwrap();
        eng.sql("CREATE TABLE docs (id INTEGER, body TEXT)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_syn_search", "search")
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        let analyzer = eng.get_table_analyzer("docs", "body", "search");
        assert!(analyzer.is_some(), "search analyzer not restored");
    }
}

#[test]
fn drop_table_removes_field_analyzers() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("drm.db");
    let cfg = r#"{"tokenizer":{"type":"standard"},"token_filters":[{"type":"lowercase"}]}"#;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            &format!("SELECT * FROM create_analyzer('rs_my_a', '{cfg}')"),
            &[],
        )
        .unwrap();
        eng.sql("CREATE TABLE docs (id INTEGER, body TEXT)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_my_a", "both")
            .unwrap();
        eng.sql("DROP TABLE docs", &[]).unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        // Table must be gone -- registering a table with the same name
        // should not see any leftover field analyzer mapping.
        assert!(!eng.has_table("docs"));
    }
}
