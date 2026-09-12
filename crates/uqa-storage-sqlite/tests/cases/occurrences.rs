//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native `SQLite` graph conformance, original-source ownership, and failure atomicity.

use std::collections::BTreeMap;
use uqa_analysis::{
    whitespace_analyzer, Analyzer, AnalyzerResources, TokenLengthPolicy, TokenTerm,
};
use uqa_storage::{
    AnalyzerPhase, InvertedIndex, MemoryInvertedIndex, RelationIdentity, TableSchema, TokenTermKey,
};
use uqa_storage_sqlite::{Catalog, ManagedConnection, SQLiteInvertedIndex};

fn fields(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("body".into(), text.into())])
}

fn config() -> Analyzer {
    serde_json::from_str(r#"{"tokenizer":{"type":"whitespace"},"token_filters":[{"type":"stop","language":"","custom_words":["gap"]},{"type":"synonym","synonyms":{"a":["a","a"]}}]}"#).unwrap()
}

fn assert_same(expected: &dyn InvertedIndex, actual: &dyn InvertedIndex, ids: &[u64]) {
    assert_eq!(actual.doc_count().unwrap(), expected.doc_count().unwrap());
    assert_eq!(
        actual.doc_length_count(None).unwrap(),
        expected.doc_length_count(None).unwrap()
    );
    assert_eq!(
        actual.field_names().unwrap(),
        expected.field_names().unwrap()
    );
    assert_eq!(
        actual.field_doc_count("body").unwrap(),
        expected.field_doc_count("body").unwrap()
    );
    assert_eq!(
        actual.total_field_length("body").unwrap(),
        expected.total_field_length("body").unwrap()
    );
    assert_eq!(
        actual.posting_count(None).unwrap(),
        expected.posting_count(None).unwrap()
    );
    assert_eq!(
        actual.vocabulary_keys("body").unwrap(),
        expected.vocabulary_keys("body").unwrap()
    );
    for id in ids {
        assert_eq!(
            actual.indexed_field_metadata(*id, "body").unwrap(),
            expected.indexed_field_metadata(*id, "body").unwrap()
        );
        assert_eq!(
            actual.get_doc_length(*id, "body").unwrap(),
            expected.get_doc_length(*id, "body").unwrap()
        );
    }
    for key in expected.vocabulary_keys("body").unwrap() {
        assert_eq!(
            actual.get_occurrence_postings("body", &key).unwrap(),
            expected.get_occurrence_postings("body", &key).unwrap()
        );
        assert_eq!(
            actual.doc_freq_key("body", &key).unwrap(),
            expected.doc_freq_key("body", &key).unwrap()
        );
        for id in ids {
            assert_eq!(
                actual.get_occurrences(*id, "body", &key).unwrap(),
                expected.get_occurrences(*id, "body", &key).unwrap()
            );
            assert_eq!(
                actual.get_term_freq_key(*id, "body", &key).unwrap(),
                expected.get_term_freq_key(*id, "body", &key).unwrap()
            );
        }
        let mut left = actual.posting_cursor_key("body", &key).unwrap();
        let mut right = expected.posting_cursor_key("body", &key).unwrap();
        loop {
            assert_eq!(left.current(), right.current());
            if left.current().is_none() {
                break;
            }
            left.advance().unwrap();
            right.advance().unwrap();
        }
    }
}

#[test]
fn persistent_batches_preserve_complete_graphs_across_clusters_and_both_length_policies() {
    for policy in [
        TokenLengthPolicy::EmittedTokens,
        TokenLengthPolicy::DiscountOverlaps,
    ] {
        check_persistent_batches(policy);
    }
}

fn check_persistent_batches(policy: TokenLengthPolicy) {
    let revision = AnalyzerResources::default()
        .compile_with_length_policy(&config(), policy)
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graphs.db");
    let conn = ManagedConnection::open(&path).unwrap();
    Catalog::open(conn.clone()).unwrap();
    let mut actual = SQLiteInvertedIndex::new(conn.clone(), "docs", whitespace_analyzer());
    let mut expected = MemoryInvertedIndex::new(whitespace_analyzer());
    let ids = [0, 1, 2, 65_535, 65_536, i64::MAX as u64];
    for index in [&mut actual as &mut dyn InvertedIndex, &mut expected] {
        index
            .set_field_analyzer_revision("body", revision.clone(), AnalyzerPhase::Both)
            .unwrap();
        index
            .try_add_documents(vec![
                (0, fields("a")),
                (1, fields("gap a gap b gap")),
                (2, fields("a a")),
                (65_535, fields("gap gap")),
                (65_536, fields("a")),
                (i64::MAX as u64, fields("")),
            ])
            .unwrap();
    }
    assert_same(&expected, &actual, &ids);
    assert_eq!(actual.get_term_freq(1, "body", "a").unwrap(), 3);
    assert_eq!(
        actual
            .get_posting_list("body", "a")
            .unwrap()
            .iter()
            .find(|entry| entry.doc_id == 1)
            .unwrap()
            .payload
            .positions,
        [1]
    );
    assert_eq!(
        actual
            .indexed_field_metadata(1, "body")
            .unwrap()
            .unwrap()
            .final_position_increment,
        1
    );
    for index in [&mut actual as &mut dyn InvertedIndex, &mut expected] {
        index
            .try_add_documents(vec![
                (1, fields("b b")),
                (2, fields("b")),
                (1, fields("gap")),
                (2, BTreeMap::new()),
                (i64::MAX as u64, fields("a")),
            ])
            .unwrap();
        index.remove_document(0).unwrap();
        index.remove_document(65_536).unwrap();
    }
    assert_same(&expected, &actual, &ids);
    drop(actual);
    let mut reopened = SQLiteInvertedIndex::new(
        ManagedConnection::open(&path).unwrap(),
        "docs",
        whitespace_analyzer(),
    );
    reopened
        .set_field_analyzer_revisions("body", revision.clone(), revision)
        .unwrap();
    assert_same(&expected, &reopened, &ids);
    let mut cursor = reopened.posting_cursor("body", "a").unwrap();
    cursor.advance_to(i64::MAX as u64).unwrap();
    assert_eq!(cursor.current().unwrap().doc_id, i64::MAX as u64);
    cursor.advance().unwrap();
    assert!(cursor.current().is_none());
    let expected_length = match policy {
        TokenLengthPolicy::EmittedTokens => 3,
        TokenLengthPolicy::DiscountOverlaps => 1,
    };
    assert_eq!(
        reopened
            .get_scoring_inputs_bulk(
                &[i64::MAX as u64, 1, i64::MAX as u64, 2],
                "body",
                &["a".into()]
            )
            .unwrap(),
        vec![
            (expected_length, vec![3]),
            (0, vec![0]),
            (expected_length, vec![3]),
            (0, vec![0])
        ]
    );
    reopened.clear().unwrap();
    assert_eq!(reopened.doc_count().unwrap(), 0);
    assert!(reopened
        .indexed_field_metadata(1, "body")
        .unwrap()
        .is_none());
}

#[test]
fn binary_nori_terms_and_source_metadata_follow_column_and_table_lifecycles() {
    let config = match serde_json::from_str::<Analyzer>(
        r#"{"char_filters":[{"type":"html_strip"}],"tokenizer":{"type":"nori_tokenizer","decompound_mode":"mixed","user_dictionary":"🙂a 가 나"},"token_filters":[]}"#,
    ) {
        Ok(config) => config,
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `nori_tokenizer`"));
            return;
        }
    };
    let conn = ManagedConnection::open_in_memory().unwrap();
    let revision = config.compile().unwrap();
    let catalog = Catalog::open(conn.clone()).unwrap();
    catalog.save_schema("public").unwrap();
    catalog
        .save_table(&TableSchema {
            relation: RelationIdentity::from_legacy_name("public.docs").unwrap(),
            role_owner: "uqa".into(),
            acl: None,
            column_acls: BTreeMap::new(),
            object_id: [1; 16],
            storage_generation: [1; 16],
            analyzer_json: serde_json::to_string(&config).unwrap(),
            fts_fields: vec!["body".into()],
            vector_fields: Vec::new(),
            columns_json: "[]".into(),
            constraints_json: String::new(),
        })
        .unwrap();
    let mut index = SQLiteInvertedIndex::new(conn.clone(), "public.docs", config);
    index.add_document(1, fields("<b>🙂a</b>")).unwrap();
    let raw = TokenTermKey::from_term(&TokenTerm::from_utf16(vec![0xd83d]));
    let edges = index.get_occurrences(1, "body", &raw).unwrap();
    assert_eq!(edges.len(), 1);
    let offsets = edges[0].offsets.unwrap();
    assert_eq!(
        (
            offsets.start_utf8,
            offsets.end_utf8,
            offsets.start_utf16,
            offsets.end_utf16
        ),
        (3, 7, 4, 5)
    );
    assert_eq!(index.stats().unwrap().doc_freq_utf16("body", &[0xd83d]), 1);
    assert_eq!(index.stats().unwrap().doc_freq("body", "�"), 0);
    assert!(index.vocabulary_terms("body").is_err());
    assert!(index
        .get_occurrences(1, "body", &TokenTermKey::from_text("🙂a"))
        .unwrap()
        .iter()
        .any(|edge| edge.position_length == 2));
    let metadata = index.indexed_field_metadata(1, "body").unwrap();
    catalog
        .rename_column_data("public.docs", "body", "caption")
        .unwrap();
    catalog
        .rename_table_data("public.docs", "public.renamed")
        .unwrap();
    let mut renamed =
        SQLiteInvertedIndex::new(conn.clone(), "public.renamed", whitespace_analyzer());
    renamed
        .set_field_analyzer_revision("caption", revision, AnalyzerPhase::Both)
        .unwrap();
    assert_eq!(renamed.get_occurrences(1, "caption", &raw).unwrap(), edges);
    assert_eq!(
        renamed.indexed_field_metadata(1, "caption").unwrap(),
        metadata
    );
    assert!(renamed.indexed_field_metadata(1, "body").unwrap().is_none());
    renamed
        .add_document(2, BTreeMap::from([("caption".into(), "<b>🙂a</b>".into())]))
        .unwrap();
    renamed.remove_document(1).unwrap();
    assert_eq!(renamed.doc_freq_key("caption", &raw).unwrap(), 1);
    catalog
        .drop_column_data("public.renamed", "caption")
        .unwrap();
    assert_eq!(renamed.doc_count().unwrap(), 0);
    assert!(renamed
        .indexed_field_metadata(2, "caption")
        .unwrap()
        .is_none());
    renamed
        .add_document(3, BTreeMap::from([("caption".into(), "🙂a".into())]))
        .unwrap();
    catalog.purge_table_data("public.renamed").unwrap();
    assert_eq!(renamed.doc_count().unwrap(), 0);
    assert_empty_graph_tables(&conn);
    renamed
        .add_document(4, BTreeMap::from([("caption".into(), "🙂a".into())]))
        .unwrap();
    catalog.drop_table_and_data("public.renamed").unwrap();
    assert_empty_graph_tables(&conn);
}

fn assert_empty_graph_tables(conn: &ManagedConnection) {
    conn.with(|db| {
        for table in [
            "_occurrence_clusters",
            "_occurrence_documents",
            "_occurrence_lengths",
            "_occurrence_fields",
            "_occurrence_formats",
        ] {
            let count: i64 = db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })?;
            assert_eq!(count, 0, "{table}");
        }
        Ok(())
    })
    .unwrap();
}

fn stored_graph(conn: &ManagedConnection) -> Vec<Vec<rusqlite::types::Value>> {
    conn.with(|db| {
        let mut rows = Vec::new();
        for table in [
            "_occurrence_clusters",
            "_occurrence_documents",
            "_occurrence_lengths",
            "_occurrence_fields",
            "_occurrence_formats",
        ] {
            let mut statement = db.prepare(&format!("SELECT * FROM {table} ORDER BY 1, 2"))?;
            let columns = statement.column_count();
            for row in statement.query_map([], |row| {
                (0..columns)
                    .map(|column| row.get(column))
                    .collect::<Result<Vec<_>, _>>()
            })? {
                rows.push(row?);
            }
        }
        Ok(rows)
    })
    .unwrap()
}

#[test]
fn graph_batch_and_rebuild_failures_restore_every_persisted_value() {
    let conn = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(conn.clone()).unwrap();
    let mut index = SQLiteInvertedIndex::new(conn.clone(), "docs", config());
    index
        .try_add_documents(vec![(1, fields("gap a gap")), (2, fields(""))])
        .unwrap();
    conn.with(|db| {
        db.execute_batch("CREATE TRIGGER reject_graph_metadata BEFORE INSERT ON _occurrence_documents WHEN NEW.doc_id = 3 BEGIN SELECT RAISE(ABORT, 'forced graph metadata failure'); END;")?;
        Ok(())
    }).unwrap();
    let before = stored_graph(&conn);
    let update = vec![(1, fields("replaced")), (3, fields("a a"))];
    assert!(index
        .try_add_documents(update.clone())
        .unwrap_err()
        .to_string()
        .contains("forced graph metadata failure"));
    assert_eq!(stored_graph(&conn), before);
    let revision = index.index_analyzer_revision("body").unwrap();
    assert!(index
        .rebuild_with_analyzer_revision(
            "body",
            whitespace_analyzer().compile().unwrap(),
            AnalyzerPhase::Both,
            update
        )
        .unwrap_err()
        .to_string()
        .contains("forced graph metadata failure"));
    assert_eq!(stored_graph(&conn), before);
    assert_eq!(
        index
            .index_analyzer_revision("body")
            .unwrap()
            .descriptor()
            .fingerprint(),
        revision.descriptor().fingerprint()
    );
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 3);
}

#[test]
fn persisted_metadata_and_score_versions_are_validated_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corrupt-graph.db");
    let conn = ManagedConnection::open(&path).unwrap();
    Catalog::open(conn.clone()).unwrap();
    let mut index = SQLiteInvertedIndex::new(conn.clone(), "docs", config());
    index.add_document(1, fields("a gap")).unwrap();
    let before = stored_graph(&conn);
    conn.with(|db| {
        db.execute("UPDATE _occurrence_lengths SET length = 999", [])?;
        Ok(())
    })
    .unwrap();
    let reopened =
        SQLiteInvertedIndex::new(ManagedConnection::open(&path).unwrap(), "docs", config());
    assert!(reopened
        .get_doc_length(1, "body")
        .unwrap_err()
        .to_string()
        .contains("source metadata"));
    assert!(reopened.get_doc_lengths_bulk(&[1, 1], "body").is_err());
    conn.with(|db| {
        db.execute("UPDATE _occurrence_lengths SET length = 3", [])?;
        db.execute("UPDATE _occurrence_documents SET metadata_blob = X'00'", [])?;
        Ok(())
    })
    .unwrap();
    assert!(reopened.indexed_field_metadata(1, "body").is_err());
    assert!(reopened
        .get_occurrences(1, "body", &TokenTermKey::from_text("a"))
        .is_err());
    let legacy = uqa_storage::clustered_postings::encode_cluster(&[
        uqa_storage::clustered_postings::ClusterPosting {
            doc_id: 1,
            term_freq: 1,
            doc_length: 1,
            positions: vec![0],
        },
    ])
    .unwrap()
    .0;
    conn.with(|db| {
        db.execute("UPDATE _occurrence_clusters SET score_blob = ?1", [legacy])?;
        Ok(())
    })
    .unwrap();
    assert!(reopened
        .posting_cursor("body", "a")
        .err()
        .unwrap()
        .to_string()
        .contains("legacy score"));
    assert!(reopened.doc_freq("body", "a").is_err());
    assert!(reopened.get_term_freq(1, "body", "a").is_err());
    assert!(reopened.stats().is_err());
    assert!(reopened.posting_count(None).is_err());
    assert_ne!(stored_graph(&conn), before);
}

#[test]
fn legacy_length_only_rows_require_an_explicit_source_rebuild() {
    let conn = ManagedConnection::open_in_memory().unwrap();
    Catalog::open(conn.clone()).unwrap();
    conn.with(|db| {
        db.execute("INSERT INTO _doc_lengths(table_name, doc_id, field, length) VALUES ('docs', 1, 'body', 0)", [])?;
        Ok(())
    }).unwrap();
    let mut index = SQLiteInvertedIndex::new(conn.clone(), "docs", config());
    assert!(index.source_rebuild_required().unwrap());
    assert!(index
        .get_doc_length(1, "body")
        .unwrap_err()
        .to_string()
        .contains("source rebuild"));
    assert!(index.add_document(2, fields("a")).is_err());
    assert!(index.remove_document(1).is_err());
    index
        .try_rebuild_documents(vec![(1, fields("gap gap"))])
        .unwrap();
    assert!(!index.source_rebuild_required().unwrap());
    assert_eq!(index.get_doc_length(1, "body").unwrap(), 0);
    assert_eq!(
        index
            .indexed_field_metadata(1, "body")
            .unwrap()
            .unwrap()
            .final_position_increment,
        2
    );
    conn.with(|db| {
        assert_eq!(
            db.query_row("SELECT count(*) FROM _doc_lengths", [], |row| row
                .get::<_, i64>(0))?,
            0
        );
        Ok(())
    })
    .unwrap();
}
