//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native graph persistence through SQL and migration from retained source text.

use std::path::Path;
use uqa_engine::{Engine, ScoringMode};
use uqa_storage::{InvertedIndex, TokenTermKey};
use uqa_storage_sqlite::{ManagedConnection, SQLiteInvertedIndex};

fn open_engine(path: &Path) -> Engine {
    Engine::open(path).unwrap()
}

#[test]
fn nori_graph_sources_survive_engine_mutation_rollback_rename_and_reopen() {
    let config = r#"{"char_filters":[{"type":"html_strip"}],"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"세종시 세종 시"},"token_filters":[]}"#;
    if let Err(error) = serde_json::from_str::<uqa_analysis::Analyzer>(config) {
        assert!(error
            .to_string()
            .contains("unknown variant `nori_tokenizer`"));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("nori-graph.db");
    let engine = open_engine(&path);
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_fts ON docs USING gin (body)", &[])
        .unwrap();
    engine
        .register_named_analyzer("korean_graph", config)
        .unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "korean_graph", "both")
        .unwrap();
    engine
        .sql("INSERT INTO docs VALUES (1, '<b>세종시</b>')", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("UPDATE docs SET body = '서울'", &[]).unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    engine
        .sql("ALTER TABLE docs RENAME COLUMN body TO caption", &[])
        .unwrap();
    engine
        .sql("ALTER TABLE docs RENAME TO renamed", &[])
        .unwrap();
    drop(engine);
    let reopened = open_engine(&path);
    assert_eq!(
        reopened
            .search("renamed", "caption", "세종", &ScoringMode::default(), 10)
            .unwrap()[0]
            .doc_id,
        1
    );
    reopened
        .sql("INSERT INTO renamed VALUES (2, '<b>세종시</b>')", &[])
        .unwrap();
    reopened
        .sql("DELETE FROM renamed WHERE id = 1", &[])
        .unwrap();
    drop(reopened);
    let index = SQLiteInvertedIndex::new(
        ManagedConnection::open(&path).unwrap(),
        "public.renamed",
        uqa_analysis::whitespace_analyzer(),
    );
    assert_eq!(index.doc_count().unwrap(), 1);
    let compound = index
        .get_occurrences(2, "caption", &TokenTermKey::from_text("세종시"))
        .unwrap();
    assert!(compound.iter().any(|edge| edge.position_length == 2));
    let source = "<b>세종시</b>";
    let metadata = index.indexed_field_metadata(2, "caption").unwrap().unwrap();
    assert_eq!(
        metadata.length_policy,
        uqa_analysis::TokenLengthPolicy::DiscountOverlaps
    );
    assert_eq!(metadata.final_offsets.end_utf8, source.len() as u64);
    assert_eq!(
        metadata.final_offsets.end_utf16,
        source.encode_utf16().count() as u64
    );
    assert!(index
        .indexed_field_metadata(1, "caption")
        .unwrap()
        .is_none());
    assert!(index.indexed_field_metadata(2, "body").unwrap().is_none());
}

#[test]
fn current_bindings_rebuild_legacy_positions_with_gaps_duplicates_and_tokenless_fields_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source-graph.db");
    let conn = legacy_source_fixture(&path);
    let catalog = uqa_storage_sqlite::Catalog::open(conn.clone()).unwrap();
    assert_eq!(
        catalog.load_table_field_analyzer_bindings().unwrap().len(),
        1
    );
    let unready = SQLiteInvertedIndex::new(
        conn.clone(),
        "public.docs",
        uqa_analysis::whitespace_analyzer(),
    );
    assert!(unready.source_rebuild_required().unwrap());
    assert!(unready.get_posting_list("body", "a").is_err());
    drop(open_engine(&path));
    let index = SQLiteInvertedIndex::new(
        conn.clone(),
        "public.docs",
        uqa_analysis::whitespace_analyzer(),
    );
    assert!(!index.source_rebuild_required().unwrap());
    assert_eq!(index.field_doc_count("body").unwrap(), 3);
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 3);
    let edges = index
        .get_occurrences(1, "body", &TokenTermKey::from_text("a"))
        .unwrap();
    assert_eq!(edges.len(), 3);
    assert!(edges
        .iter()
        .all(|edge| edge.position == 1 && edge.offsets.unwrap().start_utf8 == 4));
    let end = index.indexed_field_metadata(2, "body").unwrap().unwrap();
    assert_eq!(
        (
            end.length,
            end.final_position_increment,
            end.final_offsets.end_utf16
        ),
        (0, 2, 7)
    );
    assert!(index.indexed_field_metadata(3, "body").unwrap().is_some());
    conn.with(|db| {
        for table in [
            "_posting_clusters",
            "_posting_documents",
            "_doc_lengths",
            "_field_stats",
        ] {
            assert_eq!(
                db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))?,
                0
            );
        }
        let changed = std::collections::BTreeMap::from([
            ("id", uqa_core::Value::Int(1)),
            ("body", uqa_core::Value::Str("changed".into())),
        ]);
        db.execute(
            "UPDATE _documents SET body = ?1 WHERE table_name = 'public.docs' AND doc_id = 1",
            [serde_json::to_string(&changed).unwrap()],
        )?;
        Ok(())
    })
    .unwrap();
    let reopened = open_engine(&path);
    assert_eq!(
        reopened
            .search("docs", "body", "a", &ScoringMode::default(), 10)
            .unwrap()[0]
            .doc_id,
        1
    );
    assert_eq!(
        index
            .get_occurrences(1, "body", &TokenTermKey::from_text("a"))
            .unwrap(),
        edges
    );
}

fn legacy_source_fixture(path: &Path) -> ManagedConnection {
    let engine = open_engine(path);
    engine.sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT); CREATE INDEX docs_fts ON docs USING gin (body)", &[]).unwrap();
    engine.register_named_analyzer("graph", r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"","custom_words":["gap"]},{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#).unwrap();
    engine
        .set_table_field_analyzer("docs", "body", "graph", "both")
        .unwrap();
    engine
        .sql(
            "INSERT INTO docs VALUES (1, 'gap a gap'), (2, 'gap gap'), (3, '')",
            &[],
        )
        .unwrap();
    drop(engine);
    let conn = ManagedConnection::open(path).unwrap();
    let (scores, positions) = uqa_storage::clustered_postings::encode_cluster(&[
        uqa_storage::clustered_postings::ClusterPosting {
            doc_id: 1,
            doc_length: 1,
            term_freq: 1,
            positions: vec![0],
        },
    ])
    .unwrap();
    conn.with(|db| {
        db.execute_batch("DELETE FROM _occurrence_clusters; DELETE FROM _occurrence_documents; DELETE FROM _occurrence_lengths; DELETE FROM _occurrence_fields; DELETE FROM _occurrence_formats;
            INSERT INTO _doc_lengths(table_name, doc_id, field, length) VALUES ('public.docs', 1, 'body', 1), ('public.docs', 2, 'body', 0), ('public.docs', 3, 'body', 0);
            INSERT INTO _field_stats(table_name, field, total_length) VALUES ('public.docs', 'body', 1);")?;
        db.execute("INSERT INTO _posting_clusters(table_name, field, term, cluster_id, posting_count, score_blob, positions_blob) VALUES ('public.docs', 'body', 'a', 0, 1, ?1, ?2)", rusqlite::params![scores, positions])?;
        Ok(())
    }).unwrap();
    conn
}
