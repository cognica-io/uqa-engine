//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Original-source format migration and durable Nori graphs through the engine.

use super::{
    inverted_index_format_key, legacy_posting_key, legacy_reverse_posting_key, open_engine,
    push_key_string, Arc, Engine, KeyValueStore, RedbStorage, ScoringMode,
};
use std::path::Path;
use uqa_storage::clustered_postings::{encode_cluster, encode_terms, ClusterPosting};
use uqa_storage::{InvertedIndex, KeyValueInvertedIndex, TokenTermKey};

fn table_key(tag: u8, table: &str) -> Vec<u8> {
    let mut key = vec![tag];
    push_key_string(&mut key, table);
    key
}

fn field_key(tag: u8, table: &str, field: &str) -> Vec<u8> {
    let mut key = table_key(tag, table);
    push_key_string(&mut key, field);
    key
}

fn document_field_key(tag: u8, doc_id: u64) -> Vec<u8> {
    let mut key = table_key(tag, "public.docs");
    key.extend_from_slice(&doc_id.to_be_bytes());
    push_key_string(&mut key, "body");
    key
}

fn legacy_fixture(path: &Path, forward_rows: bool) {
    let engine = open_engine(path);
    engine
        .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
        .unwrap();
    engine
        .sql("CREATE INDEX docs_fts ON docs USING gin (body)", &[])
        .unwrap();
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
    let storage = RedbStorage::open(path).unwrap();
    let store = storage.store();
    store
        .delete_prefix(&table_key(b'e', "public.docs"))
        .unwrap();
    for (doc_id, length) in [(1, 1_u64), (2, 0), (3, 0)] {
        store
            .put(&document_field_key(b'l', doc_id), &length.to_le_bytes())
            .unwrap();
    }
    store
        .put(
            &field_key(b'f', "public.docs", "body"),
            &1_u64.to_le_bytes(),
        )
        .unwrap();
    if forward_rows {
        store.delete(&inverted_index_format_key()).unwrap();
        store
            .put(
                &legacy_posting_key("public.docs", "body", "a", 1),
                &0_u32.to_le_bytes(),
            )
            .unwrap();
        store
            .put(
                &legacy_reverse_posting_key("public.docs", 1, "body", "a"),
                &[],
            )
            .unwrap();
    } else {
        let (score_blob, positions) = encode_cluster(&[ClusterPosting {
            doc_id: 1,
            doc_length: 1,
            term_freq: 1,
            positions: vec![0],
        }])
        .unwrap();
        for (tag, bytes) in [(b'k', score_blob), (b'o', positions)] {
            let mut key = field_key(tag, "public.docs", "body");
            push_key_string(&mut key, "a");
            key.extend_from_slice(&0_u64.to_be_bytes());
            store.put(&key, &bytes).unwrap();
        }
        store
            .put(
                &document_field_key(b'x', 1),
                &encode_terms(&["a".into()]).unwrap(),
            )
            .unwrap();
        for id in [2, 3] {
            store
                .put(&document_field_key(b'x', id), &encode_terms(&[]).unwrap())
                .unwrap();
        }
    }
}

#[test]
fn complete_binding_catalog_still_rebuilds_legacy_positions_from_original_sources_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source-migration.redb");
    legacy_fixture(&path, false);
    let reopened = open_engine(&path);
    assert_eq!(
        reopened
            .search("docs", "body", "a", &ScoringMode::default(), 10)
            .unwrap()[0]
            .doc_id,
        1
    );
    drop(reopened);
    let storage = RedbStorage::open(&path).unwrap();
    let store = Arc::new(storage.store());
    let index = KeyValueInvertedIndex::new(
        store.clone(),
        "public.docs",
        uqa_analysis::whitespace_analyzer(),
    );
    assert!(!index.source_rebuild_required().unwrap());
    assert_eq!(index.doc_count().unwrap(), 3);
    assert_eq!(index.field_doc_count("body").unwrap(), 3);
    assert_eq!(index.get_term_freq(1, "body", "a").unwrap(), 3);
    let occurrences = index
        .get_occurrences(1, "body", &TokenTermKey::from_text("a"))
        .unwrap();
    assert_eq!(occurrences.len(), 3);
    assert!(occurrences
        .iter()
        .all(|edge| edge.position == 1 && edge.offsets.unwrap().start_utf8 == 4));
    let metadata = index.indexed_field_metadata(1, "body").unwrap().unwrap();
    assert_eq!(
        (
            metadata.length,
            metadata.final_offsets.end_utf8,
            metadata.final_position_increment
        ),
        (3, 9, 1)
    );
    let empty = index.indexed_field_metadata(2, "body").unwrap().unwrap();
    assert_eq!(
        (
            empty.length,
            empty.final_position_increment,
            empty.final_offsets.end_utf16
        ),
        (0, 2, 7)
    );
    assert!(index.indexed_field_metadata(3, "body").unwrap().is_some());
    for tag in [b'p', b'r', b'k', b'o', b'x', b'l', b'f'] {
        assert!(store.scan_prefix(&[tag]).unwrap().is_empty());
    }
    let graph_before = store.scan_prefix(b"e").unwrap();
    drop(index);
    drop(store);
    drop(storage);
    drop(open_engine(&path));
    let storage = RedbStorage::open(&path).unwrap();
    assert_eq!(storage.store().scan_prefix(b"e").unwrap(), graph_before);
}

#[test]
fn descriptor_failure_rolls_back_legacy_conversion_before_source_migration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("failed-source-migration.redb");
    legacy_fixture(&path, true);
    let storage = RedbStorage::open(&path).unwrap();
    let store = storage.store();
    let binding_key = field_key(b'U', "public.docs", "body");
    let binding = store.get(&binding_key).unwrap().unwrap();
    let mut malformed: serde_json::Value = serde_json::from_slice(&binding).unwrap();
    malformed["index"]["descriptor"]["fingerprint"] = serde_json::Value::String("0".repeat(64));
    store
        .put(&binding_key, &serde_json::to_vec(&malformed).unwrap())
        .unwrap();
    let before = store.scan_prefix(b"").unwrap();
    drop(store);
    drop(storage);
    let Err(error) = Engine::from_persistent_provider(Arc::new(RedbStorage::open(&path).unwrap()))
    else {
        panic!("open accepted a corrupt descriptor");
    };
    assert!(error.to_string().contains("fingerprint"), "{error}");
    let storage = RedbStorage::open(&path).unwrap();
    let store = storage.store();
    assert_eq!(store.scan_prefix(b"").unwrap(), before);
    assert!(store.get(&inverted_index_format_key()).unwrap().is_none());
    assert!(store.scan_prefix(b"e").unwrap().is_empty());
    store.put(&binding_key, &binding).unwrap();
    drop(store);
    drop(storage);
    let engine = open_engine(&path);
    assert_eq!(
        engine
            .search("docs", "body", "a", &ScoringMode::default(), 10)
            .unwrap()[0]
            .doc_id,
        1
    );
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
    let path = directory.path().join("nori-graph.redb");
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
    let storage = RedbStorage::open(&path).unwrap();
    let index = KeyValueInvertedIndex::new(
        Arc::new(storage.store()),
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
