//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn value_to_usize_rejects_non_finite_fractional_and_out_of_range_floats() {
    assert_eq!(value_to_usize(&Value::Float(42.0)).unwrap(), 42);
    for value in [f64::NAN, f64::INFINITY, -1.0, 1.5] {
        assert!(value_to_usize(&Value::Float(value)).is_err());
    }
    let exponent = i32::try_from(usize::BITS).unwrap();
    assert!(value_to_usize(&Value::Float(2.0_f64.powi(exponent))).is_err());
}

#[test]
fn persisted_ivf_parameters_reject_invalid_values() {
    let invalid = BTreeMap::from([("lists".to_string(), "not-a-number".to_string())]);
    assert!(IVFIndexParams::from_catalog_map(&invalid).is_err());

    let zero = BTreeMap::from([("probes".to_string(), "0".to_string())]);
    assert!(IVFIndexParams::from_catalog_map(&zero).is_err());
}

#[test]
fn document_id_watermark_represents_and_reports_exhaustion_without_wrapping() {
    let engine = Engine::new();
    engine.create_default_table("docs", Vec::new()).unwrap();
    let table = engine.table("docs").unwrap().expect("table");
    *table.next_id.lock() = u128::from(u64::MAX);

    assert_eq!(engine.allocate_next_id("docs").unwrap(), u64::MAX);
    let error = engine.allocate_next_id("docs").unwrap_err();
    assert!(error.to_string().contains("document id space"), "{error}");
    assert_eq!(*table.next_id.lock(), u128::from(u64::MAX) + 1);

    let second = Engine::new();
    second.create_default_table("docs", Vec::new()).unwrap();
    second.advance_next_id("docs", u64::MAX).unwrap();
    let error = second.allocate_next_id("docs").unwrap_err();
    assert!(error.to_string().contains("document id space"), "{error}");
}

#[test]
fn vector_backfill_reports_invalid_values_instead_of_skipping_them() {
    let engine = Engine::new();
    engine.create_default_table("docs", Vec::new()).unwrap();
    engine
        .add_document(
            "docs",
            1,
            doc([("embedding", Value::Str("not-a-vector".into()))]),
        )
        .unwrap();

    let error = engine
        .create_vector_field("docs", "embedding", 2)
        .unwrap_err();
    assert!(error.to_string().contains("expected vector"), "{error}");
    let unregistered = engine
        .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
        .unwrap_err();
    assert!(matches!(unregistered, SQLError::TypeMismatch(_)));
}

#[test]
fn vector_field_registration_distinguishes_absence_noop_and_dimension_mismatch() {
    let engine = Engine::new();
    let missing = engine
        .create_vector_field("missing", "embedding", 2)
        .unwrap_err();
    assert!(missing.to_string().contains("does not exist"), "{missing}");

    engine.create_default_table("docs", Vec::new()).unwrap();
    assert!(engine.create_vector_field("docs", "embedding", 2).unwrap());
    assert!(!engine.create_vector_field("docs", "embedding", 2).unwrap());
    let mismatch = engine
        .create_vector_field("docs", "embedding", 3)
        .unwrap_err();
    assert!(mismatch.to_string().contains("dimension 2"), "{mismatch}");
    assert!(mismatch.to_string().contains("requested 3"), "{mismatch}");
}

#[test]
fn direct_vector_writes_reject_unknown_tables_and_unregistered_fields() {
    let engine = Engine::new();
    let missing = engine
        .add_vector("missing", 1, "embedding", vec![1.0, 0.0])
        .unwrap_err();
    assert!(matches!(missing, SQLError::UnknownTable(_)));

    engine.create_default_table("docs", Vec::new()).unwrap();
    let unregistered = engine
        .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
        .unwrap_err();
    assert!(matches!(unregistered, SQLError::TypeMismatch(_)));
    let unregistered_many = engine
        .add_vector_values("docs", 1, "embedding", vec![vec![1.0, 0.0]])
        .unwrap_err();
    assert!(matches!(unregistered_many, SQLError::TypeMismatch(_)));

    assert!(engine.create_vector_field("docs", "embedding", 2).unwrap());
    assert!(engine
        .add_vector("docs", 1, "embedding", vec![1.0, 0.0])
        .unwrap());
    assert!(engine
        .add_vector_values("docs", 1, "embedding", vec![vec![0.0, 1.0]])
        .unwrap());
}

#[test]
fn table_introspection_distinguishes_unknown_tables_from_missing_columns() {
    let engine = Engine::new();
    for error in [
        engine.try_table_columns("missing").unwrap_err(),
        engine.try_table_has_column("missing", "value").unwrap_err(),
        engine.column_type("missing", "value").unwrap_err(),
    ] {
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    engine.create_default_table("docs", Vec::new()).unwrap();
    assert!(engine.try_table_columns("docs").unwrap().is_empty());
    assert!(!engine.try_table_has_column("docs", "value").unwrap());
    assert_eq!(engine.column_type("docs", "value").unwrap(), None);

    let sql_error = engine.sql("SELECT * FROM missing", &[]).unwrap_err();
    assert!(sql_error.to_string().contains("missing"), "{sql_error}");
    assert!(
        sql_error.to_string().contains("does not exist"),
        "{sql_error}"
    );
}

#[test]
fn direct_schema_mutations_reject_missing_relations_columns_and_duplicates() {
    let engine = Engine::new();
    let column = uqa_sql::ast::ColumnDef {
        name: "value".into(),
        ty: uqa_sql::ast::ColumnType::Integer,
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    };

    for error in [
        engine
            .register_column("missing", column.clone())
            .unwrap_err(),
        engine
            .set_column_default("missing", "value", None)
            .unwrap_err(),
        engine
            .set_column_not_null("missing", "value", true)
            .unwrap_err(),
        engine
            .set_column_type("missing", "value", &uqa_sql::ast::ColumnType::Boolean)
            .unwrap_err(),
        engine
            .try_column_default_expr("missing", "value")
            .unwrap_err(),
        engine.advance_next_id("missing", 1).unwrap_err(),
        engine
            .refresh_value_indexes_for_table("missing")
            .unwrap_err(),
        engine.try_persist_table_schema("missing").unwrap_err(),
        engine
            .try_rebuild_vector_index_for_column("missing", "embedding", 2)
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    engine.create_default_table("docs", Vec::new()).unwrap();
    engine.register_column("docs", column.clone()).unwrap();
    let duplicate = engine.register_column("docs", column).unwrap_err();
    assert!(
        duplicate.to_string().contains("already exists"),
        "{duplicate}"
    );
    for error in [
        engine
            .set_column_default("docs", "absent", None)
            .unwrap_err(),
        engine
            .set_column_not_null("docs", "absent", true)
            .unwrap_err(),
        engine
            .set_column_type("docs", "absent", &uqa_sql::ast::ColumnType::Boolean)
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("column `absent`"), "{error}");
    }

    assert!(engine.set_column_default("docs", "value", None).unwrap());
    assert!(engine.set_column_not_null("docs", "value", true).unwrap());
    assert!(engine
        .set_column_type("docs", "value", &uqa_sql::ast::ColumnType::Boolean)
        .unwrap());
    assert_eq!(
        engine.column_type("docs", "value").unwrap(),
        Some(uqa_sql::ast::ColumnType::Boolean)
    );
}

#[test]
fn table_metadata_getters_reject_unknown_relations() {
    let engine = Engine::new();
    assert!(engine.describe_table("missing").unwrap().is_none());
    for error in [
        engine.auto_increment_column("missing").unwrap_err(),
        engine.try_check_constraints("missing").unwrap_err(),
        engine.try_foreign_keys("missing").unwrap_err(),
        engine.try_unique_columns("missing").unwrap_err(),
        engine.try_key_constraints("missing").unwrap_err(),
        engine.try_referrers_to("missing").unwrap_err(),
        engine.try_column_stats("missing").unwrap_err(),
    ] {
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    engine.create_default_table("docs", Vec::new()).unwrap();
    assert_eq!(engine.auto_increment_column("docs").unwrap(), None);
    assert!(engine.try_check_constraints("docs").unwrap().is_empty());
    assert!(engine.try_foreign_keys("docs").unwrap().is_empty());
    assert!(engine.try_unique_columns("docs").unwrap().is_empty());
    assert!(engine.try_key_constraints("docs").unwrap().is_empty());
    assert!(engine.try_referrers_to("docs").unwrap().is_empty());
    assert!(engine.try_column_stats("docs").unwrap().is_empty());
}

#[test]
fn document_mutations_distinguish_unknown_tables_from_missing_documents() {
    let engine = Engine::new();
    let updates = BTreeMap::from([("value".to_string(), Value::Int(1))]);
    let vectors = BTreeMap::new();
    for error in [
        engine
            .update_document_fields("missing", 1, updates.clone(), vectors.clone())
            .unwrap_err(),
        engine
            .patch_document_fields("missing", 1, &updates, &vectors)
            .unwrap_err(),
        engine
            .rewrite_prepared_document("missing", 1, Document::new())
            .unwrap_err(),
        engine.delete_document("missing", 1).unwrap_err(),
    ] {
        assert!(matches!(error, SQLError::UnknownTable(_)), "{error}");
    }

    engine.create_default_table("docs", Vec::new()).unwrap();
    assert!(!engine
        .update_document_fields("docs", 1, updates.clone(), vectors.clone())
        .unwrap());
    assert!(!engine
        .patch_document_fields("docs", 1, &updates, &vectors)
        .unwrap());
    engine.delete_document("docs", 1).unwrap();
}

#[test]
fn tensor_backfill_reports_inner_dimension_mismatch_and_allows_null() {
    let tensor_column = uqa_sql::ast::ColumnDef {
        name: "embedding".into(),
        ty: uqa_sql::ast::ColumnType::Tensor(2),
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    };

    let engine = Engine::new();
    engine.create_default_table("bad", Vec::new()).unwrap();
    engine
        .register_column("bad", tensor_column.clone())
        .unwrap();
    engine
        .add_document(
            "bad",
            1,
            doc([(
                "embedding",
                Value::List(vec![Value::List(vec![Value::Float(1.0)])]),
            )]),
        )
        .unwrap();
    let error = engine
        .create_vector_field("bad", "embedding", 2)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("vector dimension mismatch: expected 2, got 1"),
        "{error}"
    );

    let nullable = Engine::new();
    nullable
        .create_default_table("nullable", Vec::new())
        .unwrap();
    nullable.register_column("nullable", tensor_column).unwrap();
    nullable
        .add_document("nullable", 1, doc([("embedding", Value::Null)]))
        .unwrap();
    assert!(nullable
        .create_vector_field("nullable", "embedding", 2)
        .unwrap());
}
