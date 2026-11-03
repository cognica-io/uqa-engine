//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::view;
use crate::{
    ast::RelationPersistence,
    catalog::definition_capture::views::durable_views,
    plan::{RelationalPlan, SourcePlan},
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity, Value,
};

#[test]
fn durable_view_capture_excludes_temporary_payloads_before_admission() {
    let mut unlogged = view("SELECT 2");
    unlogged.persistence = RelationPersistence::Unlogged;
    unlogged.object_id = [8; 16];
    let mut source = BTreeMap::from([
        (RelationIdentity::new("public", "first"), view("SELECT 1")),
        (RelationIdentity::new("public", "last"), unlogged),
    ]);
    let memory = MemoryBudget::new(32768);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let mut saved = None;
    let baseline = allocation_counter::measure(|| {
        saved = Some(durable_views(&source, &control).unwrap());
    });
    let baseline_bytes = memory.used();
    drop(saved.take());
    assert_eq!(memory.used(), 0);

    let mut temporary = view("SELECT 3");
    temporary.persistence = RelationPersistence::Temporary;
    temporary
        .options
        .push(("payload".into(), "x".repeat(1 << 20)));
    temporary.materialized_rows = vec![BTreeMap::from([(
        "temporary".into(),
        Value::Str("y".repeat(1 << 20)),
    )])];
    // Eligibility is carried by the object, independently of namespace spelling.
    source.insert(
        RelationIdentity::new("public", "z".repeat(1 << 20)),
        temporary,
    );
    let selected = allocation_counter::measure(|| {
        saved = Some(durable_views(&source, &control).unwrap());
    });
    let saved = saved.unwrap();
    assert_eq!(memory.used(), baseline_bytes);
    assert_eq!(selected.bytes_total, baseline.bytes_total);
    assert_eq!(selected.bytes_current, baseline.bytes_current);
    assert_eq!(saved.len(), 2);
    for ((name, captured), persistence) in saved.iter().zip([
        RelationPersistence::Permanent,
        RelationPersistence::Unlogged,
    ]) {
        let original = &source[name];
        assert_eq!(captured.persistence, persistence);
        assert_eq!(captured.object_id, original.object_id);
        assert_eq!(
            serde_json::to_value(&captured.query).unwrap(),
            serde_json::to_value(&original.query).unwrap()
        );
    }
    drop(saved);
    assert_eq!(memory.used(), 0);
}

#[test]
fn temporary_only_view_capture_skips_session_owners_with_zero_payload_allowance() {
    let mut temporary = view("SELECT 1 FROM docs");
    temporary.persistence = RelationPersistence::Temporary;
    let relation = crate::ast::InternalRelationId::allocate();
    let schema = crate::RowSchema::with_internal_relation_types(
        relation,
        vec![Some(crate::ColumnType::Integer)],
    );
    let rows = crate::ast::RetainedRowSource::new(relation, schema, 1).unwrap();
    let RelationalPlan::QueryBlock(block) = &mut temporary.query.root else {
        panic!("block")
    };
    block.from = Some(SourcePlan::RetainedRows(rows));
    let source = BTreeMap::from([(RelationIdentity::new("pg_temp_1", "view"), temporary)]);
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let allocation = allocation_counter::measure(|| {
        let saved = durable_views(&source, &control).unwrap();
        assert!(saved.is_empty());
    });
    assert_eq!(allocation.bytes_total, 0);
    assert_eq!(memory.used(), 0);
    token.cancel();
    assert!(matches!(
        durable_views(&source, &control),
        Err(SQLError::Cancelled(_))
    ));
    assert!(matches!(
        durable_views(&BTreeMap::new(), &control),
        Err(SQLError::Cancelled(_))
    ));
    assert_eq!(memory.used(), 0);
}
