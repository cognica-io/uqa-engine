//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::Expr, catalog::index::IndexDefinition};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn definition() -> IndexDefinition {
    IndexDefinition {
        catalog: Some(IndexCatalogIdentity {
            identity: CatalogObjectIdentity {
                object_id: [1; 16],
                oid: 20_001,
            },
            table_object_id: [2; 16],
            physical_key: "legacy.\"인덱스\\\"".into(),
        }),
        relationships: IndexRelationships {
            owning_constraint: Some([3; 16]),
            parent_index: Some([4; 16]),
        },
        included_columns: vec!["two words".into(), "\"quoted\"".into()],
        unique: true,
        nulls_not_distinct: true,
        ..IndexDefinition::default()
    }
}

#[test]
fn stored_index_metadata_preserves_original_addresses_and_column_order() {
    let definition = definition();
    let text = serde_json::to_string(&definition).unwrap();
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let decoded = metadata(Some(&text), &control).unwrap();
    assert_eq!(
        &**decoded.catalog.as_ref().unwrap(),
        definition.catalog.as_ref().unwrap()
    );
    assert_eq!(decoded.relationships, definition.relationships);
    assert!(decoded.unique && decoded.nulls_not_distinct);
    assert!(columns_match(
        decoded.included_columns,
        &definition.included_columns,
        &control
    )
    .unwrap());
    assert!(!columns_match(
        decoded.included_columns,
        &["\"quoted\"".into(), "two words".into()],
        &control
    )
    .unwrap());
    assert!(!columns_match(
        Some(r#"[{"Column":"two words"}]"#),
        &["two words".into()],
        &control
    )
    .unwrap());
    assert!(memory.used() >= definition.catalog.as_ref().unwrap().physical_key.len());
    drop(decoded);
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_metadata_borrows_large_expression_payloads_and_admits_copied_text() {
    let mut definition = definition();
    definition.predicate = Some(Box::new(Expr::Literal(Value::Str("x".repeat(4 << 20)))));
    let text = serde_json::to_string(&definition).unwrap();
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let decoded = metadata(Some(&text), &control).unwrap();
    let predicate = decoded.predicate.unwrap();
    assert!(predicate.len() > 4 << 20);
    assert!(predicate.as_ptr() >= text.as_ptr());
    assert!(predicate.as_bytes().as_ptr_range().end <= text.as_bytes().as_ptr_range().end);
    assert!(memory.used() < 4096);
    drop(decoded);
    assert_eq!(memory.used(), 0);
    definition.predicate = None;
    definition.catalog.as_mut().unwrap().physical_key = "x".repeat(4 << 20);
    let text = serde_json::to_string(&definition).unwrap();
    assert!(metadata(Some(&text), &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_metadata_rejects_missing_duplicate_and_malformed_ownership_fields() {
    let memory = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for text in [
        "{}",
        "[]",
        "null",
        "{",
        r#"{"unique":false}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"unique":true}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"unknown":1}"#,
        r#"{"unique":0,"nulls_not_distinct":false}"#,
        r#"{"unique":false,"nulls_not_distinct":null}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"catalog":{}}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"relationships":null}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"relationships":{"parent_index":[1]}}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"relationships":{"parent_index":null,"parent_index":null}}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"included_columns":null}"#,
        r#"{"unique":false,"nulls_not_distinct":false,"key_types":{}}"#,
        r#"{"unique":false,"nulls_not_distinct":false} true"#,
    ] {
        assert!(metadata(Some(text), &control).is_err(), "{text}");
        assert_eq!(memory.used(), 0);
    }
    let mut value = serde_json::to_value(definition()).unwrap();
    value["catalog"]["identity"]["object_id"] = serde_json::to_value([256_u16; 16]).unwrap();
    assert!(metadata(Some(&value.to_string()), &control).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_metadata_retains_legacy_defaults_and_both_cancellation_owners() {
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let other = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &other);
    assert!(metadata(None, &control).unwrap().catalog.is_none());
    let text = serde_json::to_string(&definition()).unwrap();
    assert!(metadata(Some(&text), &control).is_err());
    assert_eq!(memory.used(), 0);
    for cancel_original in [false, true] {
        let memory = MemoryBudget::new(8192);
        let token = CancellationToken::new();
        let other = CancellationToken::new();
        if cancel_original {
            token.cancel();
        } else {
            other.cancel();
        }
        let control = ProductionControl::new(&memory, &token, &other);
        assert!(metadata(Some(&text), &control).is_err());
        assert!(metadata(None, &control).is_err());
        assert_eq!(memory.used(), 0);
    }
}
