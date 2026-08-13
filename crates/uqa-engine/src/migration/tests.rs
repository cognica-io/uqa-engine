//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn malformed_embedded_vectors_are_migration_errors() {
    let binary_error = value_to_f32_vec(&Value::Bytes(vec![1, 2, 3])).unwrap_err();
    assert!(binary_error.to_string().contains("not divisible by 4"));

    let list_error = value_to_f32_vec(&Value::List(vec![Value::Str("bad".into())])).unwrap_err();
    assert!(list_error.to_string().contains("non-numeric"));

    let property_error = json_object_to_value_map("[]").unwrap_err();
    assert!(property_error.to_string().contains("JSON object"));
}

#[test]
fn migration_rejects_numeric_and_vector_narrowing_overflow() {
    let column = PythonColumnDef {
        name: "amount".into(),
        type_name: "numeric".into(),
        primary_key: false,
        not_null: false,
        auto_increment: false,
        default: None,
        vector_dimensions: None,
        unique: false,
        numeric_precision: Some(10),
        numeric_scale: Some(2_147_483_648),
    };
    let scale_error = rust_column_type(&column).unwrap_err();
    assert!(scale_error.to_string().contains("scale"));

    for value in [f64::MAX, f64::INFINITY, f64::NAN] {
        let error = value_to_f32_vec(&Value::List(vec![Value::Float(value)])).unwrap_err();
        assert!(error.to_string().contains("finite f32 range"));
    }

    let non_finite_blob = Value::Bytes(f32::INFINITY.to_le_bytes().to_vec());
    let error = value_to_f32_vec(&non_finite_blob).unwrap_err();
    assert!(error.to_string().contains("non-finite"));
}

#[test]
fn migration_rejects_invalid_persisted_vector_index_parameters() {
    let columns = [PythonColumnDef {
        name: "embedding".into(),
        type_name: "vector".into(),
        primary_key: false,
        not_null: false,
        auto_increment: false,
        default: None,
        vector_dimensions: Some(3),
        unique: false,
        numeric_precision: None,
        numeric_scale: None,
    }];
    for (index_type, parameter, value) in [
        ("ivf", "lists", "invalid"),
        ("ivf", "probes", "0"),
        ("ivf", "m", "4"),
        ("hnsw", "m", "invalid"),
        ("hnsw", "ef_search", "0"),
        ("hnsw", "lists", "4"),
    ] {
        let indexes = [CatalogIndex {
            name: "embedding_idx".into(),
            index_type: index_type.into(),
            table_name: "items".into(),
            columns: vec!["embedding".into()],
            parameters: BTreeMap::from([(parameter.into(), value.into())]),
        }];
        let error = infer_vector_fields("items", &columns, &indexes).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("embedding_idx"), "{message}");
        assert!(message.contains(parameter), "{message}");
    }
}

#[test]
fn python_array_types_preserve_elements_and_dimensions() {
    let mut column = PythonColumnDef {
        name: "values".into(),
        type_name: "TEXT[][]".into(),
        primary_key: false,
        not_null: false,
        auto_increment: false,
        default: None,
        vector_dimensions: None,
        unique: false,
        numeric_precision: None,
        numeric_scale: None,
    };
    assert_eq!(
        rust_column_type(&column).unwrap(),
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Text))))
    );

    column.type_name = "numeric[]".into();
    column.numeric_precision = Some(8);
    column.numeric_scale = Some(2);
    assert_eq!(
        rust_column_type(&column).unwrap(),
        ColumnType::Array(Box::new(ColumnType::Numeric {
            precision: Some(8),
            scale: Some(2),
        }))
    );

    let document = BTreeMap::from([
        (
            "tags".into(),
            Value::List(vec![Value::Int(1), Value::Int(2)]),
        ),
        (
            "numbers".into(),
            Value::List(vec![Value::Str("3".into()), Value::Str("4".into())]),
        ),
    ]);
    let columns = vec![
        ColumnDef {
            name: "tags".into(),
            ty: ColumnType::Array(Box::new(ColumnType::Text)),
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            check_name: None,
            check_enforced: true,
            references: None,
        },
        ColumnDef {
            name: "numbers".into(),
            ty: ColumnType::Array(Box::new(ColumnType::Integer)),
            primary_key: false,
            not_null: false,
            not_null_explicit: false,
            not_null_name: None,
            auto_increment: false,
            unique: false,
            default: None,
            check: None,
            check_name: None,
            check_enforced: true,
            references: None,
        },
    ];
    let document = coerce_migrated_document(document, &columns).unwrap();
    assert_eq!(
        document["tags"],
        Value::List(vec![Value::Str("1".into()), Value::Str("2".into())])
    );
    assert_eq!(
        document["numbers"],
        Value::List(vec![Value::Int(3), Value::Int(4)])
    );
}
