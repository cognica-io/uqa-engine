//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{AlterSequence, ColumnType, SequenceRestart};

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
