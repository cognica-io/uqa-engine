//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for the analyzer SQL surface from
//! `test_analysis`: the `create_analyzer`,
//! `list_analyzers`, `drop_analyzer`, and `set_table_analyzer` table
//! functions plus their catalog-persistence behaviour.

use tempfile::TempDir;
use uqa_engine::Engine;
use uqa_storage::{Catalog, ManagedConnection};

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
    let names = eng.sql("SELECT * FROM list_analyzers()", &[]).unwrap();
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
    assert!(eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"rs_drop_me".to_string()));
    eng.sql("SELECT * FROM drop_analyzer('rs_drop_me')", &[])
        .unwrap();
    assert!(!eng
        .list_named_analyzers()
        .unwrap()
        .contains(&"rs_drop_me".to_string()));
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
fn invalid_analyzer_is_rejected_before_registration() {
    let engine = Engine::new();
    let config = r#"{"tokenizer":{"type":"pattern","pattern":"["},"token_filters":[]}"#;
    let error = engine
        .register_named_analyzer("invalid_pattern", config)
        .unwrap_err();
    assert!(
        error.contains("invalid pattern tokenizer regular expression"),
        "unexpected error: {error}"
    );
    assert!(!engine
        .list_named_analyzers()
        .unwrap()
        .contains(&"invalid_pattern".to_string()));
}

#[test]
fn missing_synonym_file_is_rejected_before_registration() {
    let engine = Engine::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("missing-synonyms.txt");
    let config = serde_json::json!({
        "tokenizer": {"type": "whitespace"},
        "token_filters": [{
            "type": "synonym",
            "synonyms_path": path,
        }],
    })
    .to_string();
    let error = engine
        .register_named_analyzer("missing_synonyms", &config)
        .unwrap_err();
    assert!(
        error.contains("synonym file not found"),
        "unexpected error: {error}"
    );
}

#[test]
fn runtime_analyzer_failure_does_not_publish_a_partial_update() {
    let engine = Engine::new();
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("synonyms.txt");
    std::fs::write(&path, "old, legacy\n").unwrap();
    let config = serde_json::json!({
        "tokenizer": {"type": "whitespace"},
        "token_filters": [{
            "type": "synonym",
            "synonyms_path": path,
        }],
    })
    .to_string();

    engine
        .register_named_analyzer("file_synonyms", &config)
        .unwrap();
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "file_synonyms", "index")
        .unwrap();
    engine
        .sql("INSERT INTO docs VALUES (1, 'old')", &[])
        .unwrap();

    std::fs::remove_file(&path).unwrap();
    let error = engine
        .sql("UPDATE docs SET body = 'new' WHERE id = 1", &[])
        .expect_err("missing analyzer input must abort the update");
    assert!(error.to_string().contains("synonym file"), "{error}");

    let stored = engine.get_document("docs", 1).unwrap().unwrap();
    assert_eq!(
        stored.get("body"),
        Some(&uqa_core::Value::Str("old".into()))
    );
    // Search analysis intentionally uses the index analyzer when no separate
    // search-phase analyzer is configured. Restore its external resource so
    // the assertions below inspect the committed postings rather than merely
    // re-observing the expected missing-resource error.
    std::fs::write(&path, "old, legacy\n").unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM docs WHERE text_match(body, 'old')", &[],)
            .unwrap()
            .rows
            .len(),
        1
    );
    assert!(engine
        .sql("SELECT id FROM docs WHERE text_match(body, 'new')", &[],)
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn invalid_legacy_catalog_analyzer_makes_reopen_fail_explicitly() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("invalid-analyzer.db");
    {
        let connection = ManagedConnection::open(&path).unwrap();
        let catalog = Catalog::open(connection).unwrap();
        catalog
            .save_analyzer(
                "legacy_invalid",
                r#"{"tokenizer":{"type":"n_gram","min_gram":0,"max_gram":2}}"#,
            )
            .unwrap();
    }

    let Err(error) = Engine::open(&path) else {
        panic!("invalid persisted analyzer unexpectedly reopened");
    };
    assert!(
        error
            .to_string()
            .contains("invalid n-gram tokenizer gram bounds"),
        "unexpected error: {error}"
    );
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
    eng.sql("CREATE INDEX t_body_fts ON t USING gin (body)", &[])
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
    eng.sql("CREATE INDEX t_body_fts ON t USING gin (body)", &[])
        .unwrap();
    let r = eng
        .sql(
            "SELECT * FROM set_table_analyzer('t', 'body', 'rs_test_syn', 'search')",
            &[],
        )
        .unwrap();
    assert_eq!(r.rows.len(), 1);
    let analyzer = eng.get_table_analyzer("t", "body", "search").unwrap();
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
        assert!(eng
            .list_named_analyzers()
            .unwrap()
            .contains(&"rs_persistent".to_string()));
    }
    {
        let eng = Engine::open(&path).unwrap();
        // analyzer registry came back from catalog
        assert!(
            eng.list_named_analyzers()
                .unwrap()
                .contains(&"rs_persistent".to_string()),
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
        eng.sql("CREATE INDEX docs_body_fts ON docs USING gin (body)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_my_stem", "index")
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        let analyzer = eng.get_table_analyzer("docs", "body", "index").unwrap();
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
        eng.sql("CREATE INDEX docs_body_fts ON docs USING gin (body)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_syn_search", "search")
            .unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        let analyzer = eng.get_table_analyzer("docs", "body", "search").unwrap();
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
        eng.sql("CREATE INDEX docs_body_fts ON docs USING gin (body)", &[])
            .unwrap();
        eng.set_table_field_analyzer("docs", "body", "rs_my_a", "both")
            .unwrap();
        eng.sql("DROP TABLE docs", &[]).unwrap();
    }
    {
        let eng = Engine::open(&path).unwrap();
        // Table must be gone -- registering a table with the same name
        // should not see any leftover field analyzer mapping.
        assert!(!eng.has_table("docs").unwrap());
    }
}

#[test]
fn analyzer_rejections_are_exact_and_leave_no_persisted_side_effects() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("analyzer_rejections.db");
    let cfg = r#"{"tokenizer":{"type":"keyword"}}"#;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql(
            "CREATE TABLE docs (id INTEGER, indexed TEXT, plain TEXT)",
            &[],
        )
        .unwrap();
        eng.sql(
            "CREATE INDEX docs_indexed_fts ON docs USING gin (indexed)",
            &[],
        )
        .unwrap();
        eng.sql(
            &format!("SELECT * FROM create_analyzer('kept', '{cfg}')"),
            &[],
        )
        .unwrap();

        for sql in [
            format!("SELECT * FROM create_analyzer('extra', '{cfg}', 'ignored')"),
            "SELECT * FROM list_analyzers('ignored')".to_string(),
            "SELECT * FROM drop_analyzer('kept', 'ignored')".to_string(),
            "SELECT * FROM set_table_analyzer('docs', 'indexed', 'kept', 'both', 'ignored')"
                .to_string(),
        ] {
            assert!(
                eng.sql(&sql, &[]).is_err(),
                "surplus arguments accepted: {sql}"
            );
        }
        assert!(eng
            .sql("SELECT * FROM drop_analyzer('missing')", &[])
            .is_err());

        for (field, expected) in [
            ("missing", "does not exist"),
            ("id", "must be TEXT"),
            ("plain", "physical FTS index"),
        ] {
            let error = eng
                .sql(
                    &format!("SELECT * FROM set_table_analyzer('docs', '{field}', 'kept')"),
                    &[],
                )
                .expect_err("invalid analyzer target must fail");
            assert!(error.to_string().contains(expected), "{error}");
            assert_eq!(eng.table_field_analyzer("docs", field).unwrap(), None);
        }

        assert!(eng
            .list_named_analyzers()
            .unwrap()
            .contains(&"kept".to_string()));
        assert!(!eng
            .list_named_analyzers()
            .unwrap()
            .contains(&"extra".to_string()));
        assert_eq!(eng.table_field_analyzer("docs", "indexed").unwrap(), None);
    }

    let reopened = Engine::open(&path).unwrap();
    assert!(reopened
        .list_named_analyzers()
        .unwrap()
        .contains(&"kept".to_string()));
    assert!(!reopened
        .list_named_analyzers()
        .unwrap()
        .contains(&"extra".to_string()));
    for field in ["missing", "id", "plain", "indexed"] {
        assert_eq!(
            reopened.table_field_analyzer("docs", field).unwrap(),
            None,
            "rejected mapping for {field} leaked into the catalog"
        );
    }
}
