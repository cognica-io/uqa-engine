//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{AlterSequence, ColumnType, FunctionBinding, SequenceRestart, Statement};

#[test]
fn regclass_scalar_and_array_names_preserve_type_identity() {
    assert_eq!(
        ColumnType::from_sql_name("pg_catalog.regclass").unwrap(),
        ColumnType::Regclass
    );
    assert_eq!(
        ColumnType::from_sql_name("_regclass").unwrap(),
        ColumnType::Array(Box::new(ColumnType::Regclass))
    );
    assert_eq!(ColumnType::Regclass.sql_name(), "regclass");
}

#[test]
fn regtype_output_omits_type_modifiers() {
    assert_eq!(
        ColumnType::Varchar(Some(7)).regtype_name(),
        "character varying"
    );
    assert_eq!(
        ColumnType::Numeric {
            precision: Some(10),
            scale: Some(2),
        }
        .regtype_name(),
        "numeric"
    );
    assert_eq!(ColumnType::Vector(3).regtype_name(), "vector");
    assert_eq!(
        ColumnType::Array(Box::new(ColumnType::Character(4))).regtype_name(),
        "character[]"
    );
}

#[test]
fn alter_sequence_restart_reads_legacy_and_current_serde_shapes() {
    let omitted: AlterSequence = serde_json::from_str(r#"{"name":"s"}"#).unwrap();
    assert_eq!(omitted.restart, SequenceRestart::Unchanged);

    let legacy_none: AlterSequence =
        serde_json::from_str(r#"{"name":"s","restart":null}"#).unwrap();
    assert_eq!(legacy_none.restart, SequenceRestart::Unchanged);

    let legacy_value: AlterSequence = serde_json::from_str(r#"{"name":"s","restart":7}"#).unwrap();
    assert_eq!(legacy_value.restart, SequenceRestart::With(7));

    let current = AlterSequence {
        name: "s".into(),
        restart: SequenceRestart::FromStart,
        ..AlterSequence::default()
    };
    let round_trip: AlterSequence =
        serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
    assert_eq!(round_trip.restart, SequenceRestart::FromStart);
}

#[test]
fn function_binding_builtin_identity_is_backward_compatible() {
    let legacy: FunctionBinding =
        serde_json::from_str(r#"{"name":"app.f","argument_types":["text"]}"#).unwrap();
    assert!(!legacy.builtin);

    let builtin = FunctionBinding {
        name: "pg_catalog.reverse".into(),
        argument_types: vec!["text".into()],
        builtin: true,
    };
    let encoded = serde_json::to_string(&builtin).unwrap();
    assert!(encoded.contains(r#""builtin":true"#));
    assert_eq!(
        serde_json::from_str::<FunctionBinding>(&encoded).unwrap(),
        builtin
    );
}

#[test]
fn create_table_as_reads_legacy_statements_without_optional_fields() {
    let mut statement = crate::compile("CREATE TABLE copy AS SELECT 1")
        .unwrap()
        .remove(0);
    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = &statement
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(column_names.is_empty());
    assert!(!with_no_data);

    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = &mut statement
    else {
        unreachable!();
    };
    column_names.push("renamed".into());
    *with_no_data = true;
    let mut encoded = serde_json::to_value(statement).unwrap();
    let fields = encoded["CreateTableAs"].as_object_mut().unwrap();
    fields.remove("column_names");
    fields.remove("with_no_data");
    let legacy: Statement = serde_json::from_value(encoded).unwrap();
    let Statement::CreateTableAs {
        column_names,
        with_no_data,
        ..
    } = legacy
    else {
        panic!("expected CREATE TABLE AS");
    };
    assert!(column_names.is_empty());
    assert!(!with_no_data);
}
