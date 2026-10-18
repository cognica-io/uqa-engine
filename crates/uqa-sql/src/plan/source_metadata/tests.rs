//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::InternalRelationId, ColumnType};

#[test]
fn captured_attributes_keep_caller_types_and_collations_without_exposing_names() {
    let relation = InternalRelationId::allocate();
    let source = SourcePlan::Values {
        rows: Vec::new(),
        alias: None,
        column_aliases: Vec::new(),
        internal_relation: Some(relation),
        internal_column_types: vec![None, Some(ColumnType::Text)],
        internal_column_collations: vec![None, Some(951)],
    };
    let caller = RowSchema::with_types(
        vec!["first".into(), "second".into()],
        vec![Some(ColumnType::Text); 2],
    )
    .with_column_collations([Some(950), Some(950)]);
    let captured = source
        .internal_values_schema_with_inference(Some(&caller))
        .unwrap()
        .unwrap();
    assert!(captured.columns().is_empty());
    assert_eq!(captured.physical_width(), 2);
    assert_eq!(
        captured.internal_type(relation.column(0)),
        Some(&ColumnType::Text)
    );
    assert_eq!(captured.internal_collation(relation.column(0)), Some(950));
    assert_eq!(captured.internal_collation(relation.column(1)), Some(951));
}

#[test]
fn internal_values_metadata_restores_legacy_defaults_and_rejects_invalid_collations() {
    let relation = InternalRelationId::allocate();
    let source = SourcePlan::Values {
        rows: Vec::new(),
        alias: None,
        column_aliases: Vec::new(),
        internal_relation: Some(relation),
        internal_column_types: vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
        internal_column_collations: vec![Some(950), None],
    };
    let mut encoded = serde_json::to_value(&source).unwrap();
    for (collations, expected) in [(Some(950), 950), (Some(0), 0), (None, 100)] {
        if let Some(oid) = collations {
            encoded["Values"]["internal_column_collations"] = serde_json::json!([oid, null]);
        } else {
            encoded["Values"]
                .as_object_mut()
                .unwrap()
                .remove("internal_column_collations");
        }
        let restored: SourcePlan = serde_json::from_value(encoded.clone()).unwrap();
        let schema = restored.internal_values_schema().unwrap().unwrap();
        assert_eq!(schema.physical_width(), 2);
        assert_eq!(schema.len(), 0);
        assert_eq!(
            schema.internal_collation(relation.column(0)),
            Some(expected)
        );
        assert_eq!(schema.internal_collation(relation.column(1)), Some(0));
    }
    for (collations, message) in [
        (
            serde_json::json!([950]),
            "internal VALUES collation width does not match its declared attributes",
        ),
        (
            serde_json::json!([950, 950]),
            "internal VALUES collation requires a collatable attribute type",
        ),
    ] {
        encoded["Values"]["internal_column_collations"] = collations;
        let invalid: SourcePlan = serde_json::from_value(encoded.clone()).unwrap();
        let error = invalid.internal_values_schema().unwrap_err();
        assert_eq!(error.to_string(), format!("internal error: {message}"));
    }
}
