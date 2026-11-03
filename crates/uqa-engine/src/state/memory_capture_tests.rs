//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Actual Engine publication and rollback use the storage owners' capture boundaries.

use crate::Engine;
use std::sync::Arc;
use uqa_storage::read_control::StorageReadControl;

mod analyzers;
mod catalog_authority;
mod foreign;
mod identities;
mod metadata;
mod paths;
mod relation_authority;
mod resources;
mod roles;
mod routines;
mod rules;
mod triggers;
mod types;
mod views;

fn sql(engine: &Engine, sql: &str) {
    engine.sql(sql, &[]).unwrap();
}

#[test]
fn memory_engine_capture_preserves_evaluated_expression_keys_after_routine_replacement() {
    for mutate in [false, true] {
        let engine = Engine::new();
        sql(&engine, "CREATE FUNCTION saved_key(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN lower(x); CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'Original'); CREATE INDEX saved_expression ON docs(saved_key(body))");
        let field = engine
            .value_indexable_fields("docs")
            .unwrap()
            .into_iter()
            .find(|field| matches!(field, uqa_storage::ValueIndexKey::Index(_)))
            .unwrap();
        let table = engine.table("docs").unwrap().unwrap();
        let key = table.value_indexes.key().unwrap();
        sql(&engine, "CREATE OR REPLACE FUNCTION saved_key(x text) RETURNS text IMMUTABLE LANGUAGE SQL RETURN upper(x)");
        let control = StorageReadControl::with_limit(1 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(key.retained(&cut).unwrap().is_none());
        if mutate {
            sql(&engine, "BEGIN; UPDATE docs SET body='Private'; SAVEPOINT retained; TRUNCATE docs; ROLLBACK TO retained; ROLLBACK");
            sql(
                &engine,
                "DROP TABLE docs; CREATE TABLE docs(replacement integer)",
            );
        }
        drop((table, engine));
        let captured = key.retained(&cut).unwrap().unwrap();
        let index = captured.get(&field).unwrap().unwrap();
        assert_eq!(
            index.value(1).unwrap(),
            Some(&crate::Value::Row(
                vec![crate::Value::Str("original".into())].into()
            ))
        );
        assert_eq!(index.len().unwrap(), 1);
        drop((captured, cut));
        assert_eq!(control.memory().used(), 0);
    }
}

fn document_image(source: &dyn uqa_storage::DocumentStore) -> Vec<u8> {
    let mut image = Vec::new();
    assert!(source
        .write_image_document(1, &mut image, &StorageReadControl::with_limit(1 << 20))
        .unwrap()
        .is_some());
    image
}

fn text_image(source: &dyn uqa_storage::InvertedIndex) -> Vec<u8> {
    let mut image = Vec::new();
    assert!(source
        .write_image_document(1, &mut image, &StorageReadControl::with_limit(1 << 20))
        .unwrap()
        .is_some());
    image
}

#[test]
fn memory_engine_capture_preserves_physical_sources_across_rollback_replacement_and_closure() {
    for method in ["exact", "ivf", "hnsw", "diskann"] {
        let engine = Arc::new(Engine::new());
        sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text, embedding vector(2)); CREATE INDEX text_idx ON docs USING gin(body); INSERT INTO docs VALUES(1,'original term',ARRAY[1.0,0.0]),(2,'second',ARRAY[0.0,1.0])");
        if method != "exact" {
            sql(
                &engine,
                &format!("CREATE INDEX vector_idx ON docs USING {method}(embedding)"),
            );
        }
        let table = engine.table("docs").unwrap().unwrap();
        let documents = document_image(table.document_store.read().as_ref());
        let text = text_image(table.inverted_index.read().as_ref());
        let mut vectors = Vec::new();
        table.vector_indexes.read()["embedding"]
            .write_image(&mut vectors, &StorageReadControl::with_limit(16 << 20))
            .unwrap();
        let document_key = table.document_store.key().unwrap();
        let text_key = table.inverted_index.key().unwrap();
        let vector_key = table.vector_indexes.key().unwrap();
        let control = StorageReadControl::with_limit(32 << 20);
        let (cut, membership) = {
            let _statement = engine.lock_statement_gate();
            (
                engine
                    .storage
                    .memory_capture
                    .as_ref()
                    .unwrap()
                    .capture(&control)
                    .unwrap(),
                engine.storage.tables.live_snapshot(),
            )
        };
        assert!(control.memory().used() < 4096);
        let writer = engine.clone();
        std::thread::spawn(move || {
            sql(&writer, "BEGIN; UPDATE docs SET body='private',embedding=ARRAY[-1.0,0.0] WHERE id=1; SAVEPOINT saved; TRUNCATE docs; ROLLBACK TO saved; ROLLBACK");
            sql(&writer, "UPDATE docs SET body='committed',embedding=ARRAY[0.5,0.5] WHERE id=1; DROP TABLE docs; CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,'replacement')");
        }).join().unwrap();
        let name = uqa_core::RelationIdentity::from_legacy_name("docs").unwrap();
        assert!(Arc::ptr_eq(&membership[&name], &table));
        assert!(!Arc::ptr_eq(&engine.storage.tables.read()[&name], &table));
        let captured_documents = table.document_store.captured(&cut).unwrap();
        let captured_text = table.inverted_index.captured(&cut).unwrap();
        let captured_vectors = table.vector_indexes.captured(&cut).unwrap();
        assert_eq!(
            document_image(&***captured_documents),
            documents,
            "{method}"
        );
        assert_eq!(text_image(&***captured_text), text, "{method}");
        let image = captured_vectors.image("embedding", &cut).unwrap();
        let mut bytes = Vec::new();
        image
            .write_image(&mut bytes, &StorageReadControl::with_limit(16 << 20))
            .unwrap();
        assert_eq!(bytes, vectors, "{method}");
        drop((table, membership, engine));
        bytes.clear();
        image
            .write_image(&mut bytes, &StorageReadControl::with_limit(16 << 20))
            .unwrap();
        assert_eq!(bytes, vectors, "{method}");
        assert!(Arc::ptr_eq(
            &document_key.retained(&cut).unwrap().unwrap(),
            &captured_documents
        ));
        assert!(Arc::ptr_eq(
            &text_key.retained(&cut).unwrap().unwrap(),
            &captured_text
        ));
        assert!(Arc::ptr_eq(
            &vector_key.retained(&cut).unwrap().unwrap(),
            &captured_vectors
        ));
        drop((
            cut,
            captured_documents,
            captured_text,
            captured_vectors,
            image,
        ));
        assert_eq!(control.memory().used(), 0, "{method}");
    }
}

#[test]
fn memory_engine_capture_retains_unread_sources_before_the_last_engine_owner_closes() {
    for method in ["exact", "ivf", "hnsw", "diskann"] {
        let engine = Engine::new();
        sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text, embedding vector(2)); CREATE INDEX text_idx ON docs USING gin(body); INSERT INTO docs VALUES(1,'unchanged',ARRAY[1.0,0.0])");
        if method != "exact" {
            sql(
                &engine,
                &format!("CREATE INDEX vector_idx ON docs USING {method}(embedding)"),
            );
        }
        let table = engine.table("docs").unwrap().unwrap();
        let documents = document_image(table.document_store.read().as_ref());
        let text = text_image(table.inverted_index.read().as_ref());
        let mut expected_vector = Vec::new();
        table.vector_indexes.read()["embedding"]
            .write_image(
                &mut expected_vector,
                &StorageReadControl::with_limit(16 << 20),
            )
            .unwrap();
        let document_key = table.document_store.key().unwrap();
        let text_key = table.inverted_index.key().unwrap();
        let vector_key = table.vector_indexes.key().unwrap();
        let control = StorageReadControl::with_limit(4 << 20);
        let cut = engine
            .storage
            .memory_capture
            .as_ref()
            .unwrap()
            .capture(&control)
            .unwrap();
        assert!(document_key.retained(&cut).unwrap().is_none());
        assert!(text_key.retained(&cut).unwrap().is_none());
        assert!(vector_key.retained(&cut).unwrap().is_none());
        drop((table, engine));
        assert_eq!(
            document_image(&***document_key.retained(&cut).unwrap().unwrap()),
            documents
        );
        assert_eq!(
            text_image(&***text_key.retained(&cut).unwrap().unwrap()),
            text
        );
        let vectors = vector_key.retained(&cut).unwrap().unwrap();
        let image = vectors.image("embedding", &cut).unwrap();
        let mut bytes = Vec::new();
        image
            .write_image(&mut bytes, &StorageReadControl::with_limit(1 << 20))
            .unwrap();
        assert_eq!(bytes, expected_vector, "{method}");
        let restored = uqa_storage::vector_index::image::read_vector_image(
            &mut bytes.as_slice(),
            &StorageReadControl::with_limit(1 << 20),
        )
        .unwrap();
        assert_eq!(restored.count().unwrap(), 1);
        drop((restored, image, vectors, cut));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_engine_capture_membership_registration_has_no_relation_or_document_copy() {
    for count in [1, 128] {
        let engine = Engine::new();
        for id in 0..count {
            sql(&engine, &format!("CREATE TABLE t{id}(id integer PRIMARY KEY, body text); INSERT INTO t{id} VALUES(1,repeat('x',8192))"));
        }
        let registry = engine.storage.memory_capture.as_ref().unwrap();
        let control = StorageReadControl::with_limit(1 << 20);
        let mut captured = None;
        let allocated = allocation_counter::measure(|| {
            let _statement = engine.lock_statement_gate();
            captured = Some((
                registry.capture(&control).unwrap(),
                engine.storage.tables.live_snapshot(),
            ));
        });
        assert!(allocated.bytes_total < 4096, "{count}: {allocated:?}");
        let (cut, tables) = captured.unwrap();
        assert_eq!(tables.len(), count);
        assert!(Arc::ptr_eq(&tables, &engine.storage.tables.live_snapshot()));
        sql(&engine, "CREATE TABLE later(id integer)");
        assert_eq!(tables.len(), count);
        assert_eq!(engine.storage.tables.read().len(), count + 1);
        assert!(!Arc::ptr_eq(
            &tables,
            &engine.storage.tables.live_snapshot()
        ));
        drop((cut, tables));
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn memory_engine_capture_failure_preserves_transaction_progress_and_other_cuts() {
    let engine = Engine::new();
    sql(&engine, "CREATE TABLE docs(id integer PRIMARY KEY, body text); INSERT INTO docs VALUES(1,repeat('x',65536))");
    let table = engine.table("docs").unwrap().unwrap();
    let expected = document_image(table.document_store.read().as_ref());
    let small = StorageReadControl::with_limit(4096);
    let good = StorageReadControl::with_limit(1 << 20);
    let registry = engine.storage.memory_capture.as_ref().unwrap();
    let failed = registry.capture(&small).unwrap();
    let saved = registry.capture(&good).unwrap();
    sql(&engine, "UPDATE docs SET body='new'");
    sql(&engine, "BEGIN; DELETE FROM docs; ROLLBACK");
    assert!(failed.check().is_err());
    assert_eq!(
        document_image(&***table.document_store.captured(&saved).unwrap()),
        expected
    );
    assert_eq!(
        engine.sql("SELECT body FROM docs", &[]).unwrap().rows[0]["body"],
        uqa_core::Value::Str("new".into())
    );
    drop((failed, saved));
    assert_eq!(small.memory().used(), 0);
    assert_eq!(good.memory().used(), 0);
}

#[test]
fn memory_engine_capture_excludes_temporary_table_payloads() {
    let definition = "CREATE TEMP TABLE temp_docs(id integer, body text, embedding vector(2)); CREATE INDEX temp_text ON temp_docs USING gin(body)";
    let engine = Engine::new();
    sql(&engine, definition);
    let baseline = StorageReadControl::with_limit(1 << 20);
    let empty_cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&baseline)
        .unwrap();
    sql(
        &engine,
        "INSERT INTO temp_docs VALUES(1,repeat('x',65536),ARRAY[1.0,0.0])",
    );
    let table = engine.table("temp_docs").unwrap().unwrap();
    assert!(table.document_store.key().is_err());
    assert!(table.inverted_index.key().is_err());
    assert!(table.vector_indexes.key().is_err());
    assert!(table.value_indexes.key().is_err());
    assert!(table.columns.key().is_err());
    assert!(table.hierarchy.key().is_err());
    assert!(table.dropped_attributes.key().is_err());
    // Both cuts contain the same catalog definitions; populated temporary stores must add no capture retention.
    let control = StorageReadControl::with_limit(1 << 20);
    let cut = engine
        .storage
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&control)
        .unwrap();
    sql(&engine, "UPDATE temp_docs SET body='changed',embedding=ARRAY[0.0,1.0]; TRUNCATE temp_docs; DROP TABLE temp_docs");
    drop((table, engine));
    empty_cut.check().unwrap();
    cut.check().unwrap();
    assert_eq!(control.memory().used(), baseline.memory().used());
    drop((cut, empty_cut));
    assert_eq!(baseline.memory().used(), 0);
    assert_eq!(control.memory().used(), 0);
}
