//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeMap, sync::atomic::Ordering};
use uqa_core::{Predicate, RelationIdentity};
use uqa_sql::{
    ast::{Expr, IndexKey},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
};
use uqa_storage::CatalogIndexRow;

mod fixture;
use fixture::Documents;

fn hydration<'a>(
    documents: &'a Documents,
    definitions: &'a PhysicalIndexDefinitions,
    field: &'a ValueIndexKey,
    control: &'a StorageReadControl,
    bytes: usize,
) -> ValueIndexHydration<'a> {
    ValueIndexHydration {
        documents,
        definitions,
        expressions: IndexExpressionContext {
            catalog: documents,
            values: None,
            expressions: documents,
        },
        table: "public.t",
        field,
        work_mem_bytes: bytes,
        control,
    }
}

fn expression_definition() -> PhysicalIndexDefinitions {
    let row = CatalogIndexRow {
        relation: RelationIdentity::new("public", "expression"),
        table_name: "public.t".into(),
        index_type: "btree".into(),
        columns_json: serde_json::to_string(&[IndexKey::Expression(Box::new(Expr::Column(
            "key".into(),
        )))])
        .unwrap(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                        object_id: [2; 16],
                        oid: 17001,
                    },
                    table_object_id: [1; 16],
                    physical_key: "physical-expression".into(),
                }),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    };
    PhysicalIndexDefinitions::prepare(&BTreeMap::from([(row.relation.clone(), row)])).unwrap()
}

#[test]
fn cold_index_capture_reconciles_stale_and_missing_ids_without_reevaluating_stored_keys() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let definitions = PhysicalIndexDefinitions::default();
    let field = ValueIndexKey::Column("key".into());
    for bytes in [0, 64 * 1024] {
        let documents = Documents::new(3, 0, None, false);
        let index = hydration(&documents, &definitions, &field, &control, bytes)
            .build(false, |visit| {
                assert_eq!(documents.reads.pages.load(Ordering::Relaxed), 0);
                visit(0, Value::Int(999))?;
                visit(1, Value::Str("original stored key".into()))?;
                visit(3, Value::Float(-0.0))?;
                visit(u64::MAX, Value::Null)?;
                Ok(true)
            })
            .unwrap();
        assert_eq!(*documents.reads.documents.lock(), [2]);
        assert_eq!(
            index.stored_value(1, &control).unwrap().as_deref(),
            Some(&Value::Str("original stored key".into()))
        );
        assert_eq!(
            index.stored_value(2, &control).unwrap().as_deref(),
            Some(&Value::Int(2))
        );
        assert!(index
            .stored_value(3, &control)
            .unwrap()
            .unwrap()
            .has_same_representation(&Value::Float(-0.0)));
        assert!(!index.contains(0, &control).unwrap());
        assert!(!index.contains(u64::MAX, &control).unwrap());
    }
}

#[test]
fn cold_index_expression_capture_preserves_batch_boundary_and_releases_document_guards() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let definitions = expression_definition();
    let field = ValueIndexKey::Index("physical-expression".into());
    let documents = Documents::new(259, 0, None, false);
    let index = hydration(&documents, &definitions, &field, &control, 4096)
        .build(false, |_| Ok(false))
        .unwrap();
    assert_eq!(documents.reads.evaluations.load(Ordering::Relaxed), 259);
    assert_eq!(
        index.stored_value(259, &control).unwrap().as_deref(),
        Some(&Value::Row(vec![Value::Int(259)].into()))
    );
    assert!(index.is_spilled());
}

#[test]
fn cold_index_capture_preserves_later_read_errors_before_validation_or_evaluation() {
    let control = StorageReadControl::with_limit(64 * 1024);
    for expression in [false, true] {
        let definitions = expression_definition();
        let field = if expression {
            ValueIndexKey::Index("physical-expression".into())
        } else {
            ValueIndexKey::Column("key".into())
        };
        let documents = Documents::new(3, 0, Some(2), true);
        let error = hydration(&documents, &definitions, &field, &control, 0)
            .build(false, |_| Ok(false))
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert!(error.to_string().contains("later document read"));
        assert_eq!(documents.reads.evaluations.load(Ordering::Relaxed), 0);
        assert_eq!(*documents.reads.documents.lock(), [1, 2]);
    }
}

#[test]
fn cold_index_capture_handles_empty_markers_cancellation_and_provider_contract_errors() {
    let definitions = PhysicalIndexDefinitions::default();
    let field = ValueIndexKey::Column("key".into());
    for built in [false, true] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let documents = Documents::new(0, 0, None, false);
        let index = hydration(&documents, &definitions, &field, &control, 0)
            .build(false, |_| Ok(built))
            .unwrap();
        assert_eq!(
            index
                .estimate_cardinality(&Predicate::IsNotNull, &control)
                .unwrap(),
            Some(0)
        );
    }
    for failure in [0, 1, 2] {
        let control = StorageReadControl::with_limit(64 * 1024);
        let documents = Documents::new(3, 0, None, false);
        let error = hydration(&documents, &definitions, &field, &control, 0)
            .build(false, |visit| {
                visit(1, Value::Int(7))?;
                match failure {
                    0 => {
                        visit(1, Value::Int(8))?;
                    }
                    1 => return Ok(false),
                    _ => control.cancellation().cancel(),
                }
                Ok(true)
            })
            .err()
            .unwrap();
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 2 { "57014" } else { "XX000" })
        );
        assert!(documents.reads.documents.lock().is_empty());
    }
}

#[test]
fn cold_index_capture_bounds_wide_missing_key_population_before_cache_publication() {
    let control = StorageReadControl::with_limit(64 * 1024);
    let definitions = PhysicalIndexDefinitions::default();
    let field = ValueIndexKey::Column("key".into());
    let documents = Documents::new(256, 8192, None, false);
    let mut index = None;
    let measured = allocation_counter::measure(|| {
        index = Some(
            hydration(&documents, &definitions, &field, &control, 64 * 1024)
                .build(false, |_| Ok(false))
                .unwrap(),
        );
    });
    assert!(measured.bytes_max <= 1536 * 1024, "{measured:?}");
    let index = index.unwrap();
    assert!(index.is_spilled());
    assert_eq!(
        index
            .estimate_cardinality(&Predicate::IsNotNull, &control)
            .unwrap(),
        Some(256)
    );
    assert_eq!(documents.reads.documents.lock().len(), 256);
}
