//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A retained visible maximum survives private undo, source retirement and later writes.

use super::{expect, expect_eq};
use crate::{
    document_store::maximum::MaximumDocIdStep, read_control::StorageReadControl,
    PersistentStorageSession, RelationIdentity, StorageBackendResult,
};
use std::collections::BTreeMap;

const TABLE: &str = "\"Maximum.Schema\".\"Maximum.Table\"";

/// Exercise key-only progress, tombstones, retained private visibility, admission and cancellation on a fresh disposable persistent session.
pub fn verify_document_maximum_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    source.catalog.initialize_storage()?;
    source
        .catalog
        .save_schema_row(&crate::SchemaRow::bootstrap("Maximum.Schema"))?;
    source.catalog.save_table(&crate::TableSchema {
        relation: RelationIdentity::new("Maximum.Schema", "Maximum.Table"),
        security: crate::RelationSecurityRow::bootstrap(),
        object_id: [31; 16],
        storage_generation: [32; 16],
        analyzer_json: "{}".into(),
        fts_fields: vec![],
        vector_fields: vec![],
        columns_json: "[]".into(),
        constraints_json: "{}".into(),
    })?;
    let mut documents = source.backend.document_store(TABLE);
    expect_eq(&documents.max_doc_id()?, &0, "empty visible maximum")?;
    let payload: crate::document_store::Document =
        [("large".into(), uqa_core::Value::Str("x".repeat(128 << 10)))].into();
    for id in [1, 5, 9, 15, 17, 21] {
        documents.put(id, payload.clone())?;
    }
    source.backend.begin_transaction()?;
    for id in [15, 17, 21] {
        documents.delete(id)?;
    }
    documents.put(13, payload)?;
    expect_eq(
        &documents.max_doc_id()?,
        &13,
        "private visible maximum excludes reservations",
    )?;
    let control = StorageReadControl::with_limit(65536);
    let mut input = documents.begin_max_doc_id_input(4096, &control)?;
    let retained = documents.snapshot()?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later
        .backend
        .document_store(TABLE)
        .put(29, BTreeMap::new())?;
    let owner = std::sync::Arc::downgrade(&source.backend);
    drop(documents);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "maximum input releases its originating backend",
    )?;
    let mut pending = 0;
    let mut complete = false;
    for _ in 0..16 {
        match input.advance()? {
            MaximumDocIdStep::Pending => pending += 1,
            MaximumDocIdStep::Complete(value) => {
                expect_eq(
                    &value,
                    &13,
                    "original private maximum survives source retirement",
                )?;
                complete = true;
                break;
            }
        }
    }
    expect(
        complete && pending >= 3,
        "deleted slots each consume a progress step",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "completed maximum releases every input allocation",
    )?;
    expect_eq(
        &retained.max_doc_id()?,
        &13,
        "ordinary retained maximum agrees",
    )?;
    let mut again = retained.begin_max_doc_id_input(4096, &control)?;
    for _ in 0..16 {
        if let MaximumDocIdStep::Complete(value) = again.advance()? {
            expect_eq(&value, &13, "retained handle never captures a newer view")?;
            break;
        }
    }
    expect_eq(&control.memory().used(), &0, "retained input completes")?;
    control.cancellation().cancel();
    expect_eq(
        &input.advance()?,
        &MaximumDocIdStep::Complete(13),
        "completion is idempotent",
    )?;
    drop(retained);
    verify_retirement(&later)
}

fn verify_retirement(later: &PersistentStorageSession) -> StorageBackendResult<()> {
    let documents = later.backend.document_store(TABLE);
    for limit in [0, 1, 32, 65536] {
        let control = StorageReadControl::with_limit(limit);
        let result = documents.begin_max_doc_id_input(4096, &control);
        if limit < 32 {
            expect(
                result.is_err(),
                "maximum construction admits its allocations",
            )?;
        }
        drop(result);
        expect_eq(
            &control.memory().used(),
            &0,
            "abandoned maximum retires its allocations",
        )?;
    }
    for bound in [0, 1] {
        let control = StorageReadControl::with_limit(65536);
        expect(
            documents.begin_max_doc_id_input(bound, &control).is_err(),
            "physical key bound rejects before enumeration",
        )?;
        expect_eq(&control.memory().used(), &0, "invalid bound releases input")?;
    }
    let control = StorageReadControl::with_limit(65536);
    let mut cancelled = documents.begin_max_doc_id_input(4096, &control)?;
    control.cancellation().cancel();
    expect(cancelled.advance().is_err(), "original input cancellation")?;
    expect(
        cancelled.advance().is_err(),
        "cancelled maximum is terminal",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "cancellation releases the selected view",
    )?;

    let read_control = StorageReadControl::with_limit(1 << 20);
    let selected = later.backend.open_controlled_session(&read_control)?;
    let control = StorageReadControl::with_limit(65536);
    let mut input = selected
        .backend
        .document_store(TABLE)
        .begin_max_doc_id_input(4096, &control)?;
    read_control.cancellation().cancel();
    expect(
        input.advance().is_ok(),
        "write cancellation does not invalidate retained reads",
    )?;
    drop(input);
    let mut input = selected
        .backend
        .document_store(TABLE)
        .begin_max_doc_id_input(4096, &control)?;
    selected
        .backend
        .retention_control()
        .expect("versioned read owner")
        .cancellation()
        .cancel();
    expect(
        input.advance().is_err(),
        "provider read cancellation survives retention",
    )?;
    expect_eq(
        &control.memory().used(),
        &0,
        "provider cancellation retires input",
    )?;
    Ok(())
}
