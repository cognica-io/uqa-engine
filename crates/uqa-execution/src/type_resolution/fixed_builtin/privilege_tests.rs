//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` privilege inquiry overload registration tests.

use super::*;

#[test]
fn pg_has_role_registers_every_postgresql_name_and_oid_overload() {
    let overloads = overloads("pg_has_role").unwrap();

    assert_eq!(overloads.len(), 6);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![ColumnType::Name, ColumnType::Name, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Name, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text],
        ]
    );
}

#[test]
fn has_sequence_privilege_registers_every_postgresql_name_and_oid_overload() {
    let overloads = overloads("has_sequence_privilege").unwrap();

    assert_eq!(overloads.len(), 6);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![ColumnType::Name, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text],
        ]
    );
}

#[test]
fn has_table_privilege_registers_every_postgresql_name_and_oid_overload() {
    let overloads = overloads("has_table_privilege").unwrap();

    assert_eq!(overloads.len(), 6);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![ColumnType::Name, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text],
        ]
    );
}

#[test]
fn has_column_privilege_registers_every_postgresql_overload() {
    let overloads = overloads("has_column_privilege").unwrap();

    assert_eq!(overloads.len(), 12);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![
                ColumnType::Name,
                ColumnType::Text,
                ColumnType::Text,
                ColumnType::Text
            ],
            vec![
                ColumnType::Name,
                ColumnType::Text,
                ColumnType::SmallInteger,
                ColumnType::Text
            ],
            vec![
                ColumnType::Name,
                ColumnType::Oid,
                ColumnType::Text,
                ColumnType::Text
            ],
            vec![
                ColumnType::Name,
                ColumnType::Oid,
                ColumnType::SmallInteger,
                ColumnType::Text
            ],
            vec![
                ColumnType::Oid,
                ColumnType::Text,
                ColumnType::Text,
                ColumnType::Text
            ],
            vec![
                ColumnType::Oid,
                ColumnType::Text,
                ColumnType::SmallInteger,
                ColumnType::Text
            ],
            vec![
                ColumnType::Oid,
                ColumnType::Oid,
                ColumnType::Text,
                ColumnType::Text
            ],
            vec![
                ColumnType::Oid,
                ColumnType::Oid,
                ColumnType::SmallInteger,
                ColumnType::Text
            ],
            vec![ColumnType::Text, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Text, ColumnType::SmallInteger, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::SmallInteger, ColumnType::Text],
        ]
    );
}

#[test]
fn has_database_privilege_registers_every_postgresql_name_and_oid_overload() {
    let overloads = overloads("has_database_privilege").unwrap();

    assert_eq!(overloads.len(), 6);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![ColumnType::Name, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text],
        ]
    );
}

#[test]
fn has_schema_privilege_registers_every_postgresql_name_and_oid_overload() {
    let overloads = overloads("has_schema_privilege").unwrap();

    assert_eq!(overloads.len(), 6);
    assert!(overloads
        .iter()
        .all(|overload| overload.return_type == ColumnType::Boolean));
    assert_eq!(
        overloads
            .iter()
            .map(|overload| overload.argument_types.clone())
            .collect::<Vec<_>>(),
        vec![
            vec![ColumnType::Name, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Name, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Oid, ColumnType::Text],
            vec![ColumnType::Text, ColumnType::Text],
            vec![ColumnType::Oid, ColumnType::Text],
        ]
    );
}
