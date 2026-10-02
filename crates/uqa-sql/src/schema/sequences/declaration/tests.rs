//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::SequenceOptionValue::{Absent, Integer, Text};

fn failure(declaration: &SequenceDeclaration, data_type: &ColumnType, identity: bool) -> String {
    let error = declare_sequence(declaration, data_type, identity).unwrap_err();
    format!("{} {error}", error.sqlstate().unwrap_or_default())
}

#[test]
fn omitted_options_follow_the_type_and_the_direction_of_the_increment() {
    let ascending =
        declare_sequence(&SequenceDeclaration::default(), &ColumnType::Integer, true).unwrap();
    assert_eq!(
        ascending.definition,
        SequenceDefinition {
            start: 1,
            increment: 1,
            data_type: SequenceDataType::Integer,
            min_value: 1,
            max_value: i64::from(i32::MAX),
            cycle: false,
            cache_size: 1,
        }
    );
    assert_eq!(ascending.current, 1);
    let descending = declare_sequence(
        &SequenceDeclaration {
            increment: Some(Integer(-2)),
            min_value: Some(Absent),
            ..SequenceDeclaration::default()
        },
        &ColumnType::SmallInteger,
        true,
    )
    .unwrap()
    .definition;
    assert_eq!(
        (descending.min_value, descending.max_value, descending.start),
        (i64::from(i16::MIN), -1, -1)
    );
}

#[test]
fn restart_gives_the_first_value_without_moving_the_start() {
    let declared = declare_sequence(
        &SequenceDeclaration {
            start: Some(Integer(5)),
            restart: Some(Text("9".into())),
            ..SequenceDeclaration::default()
        },
        &ColumnType::BigInteger,
        false,
    )
    .unwrap();
    assert_eq!((declared.definition.start, declared.current), (5, 9));
    let bare = declare_sequence(
        &SequenceDeclaration {
            start: Some(Integer(5)),
            restart: Some(Absent),
            ..SequenceDeclaration::default()
        },
        &ColumnType::BigInteger,
        false,
    )
    .unwrap();
    assert_eq!(bare.current, 5);
}

#[test]
fn options_are_read_and_checked_in_postgresql_order() {
    // The bounds come before the start and the cache, whatever order the declaration writes.
    assert_eq!(
        failure(
            &SequenceDeclaration {
                start: Some(Text("99999999999999999999".into())),
                max_value: Some(Integer(5)),
                min_value: Some(Integer(7)),
                ..SequenceDeclaration::default()
            },
            &ColumnType::BigInteger,
            false,
        ),
        "22023 MINVALUE (7) must be less than MAXVALUE (5)"
    );
    assert_eq!(
        failure(
            &SequenceDeclaration {
                cache: Some(Integer(0)),
                start: Some(Integer(0)),
                ..SequenceDeclaration::default()
            },
            &ColumnType::Integer,
            false,
        ),
        "22023 START value (0) cannot be less than MINVALUE (1)"
    );
    assert_eq!(
        failure(
            &SequenceDeclaration {
                cache: Some(Integer(0)),
                ..SequenceDeclaration::default()
            },
            &ColumnType::Integer,
            false,
        ),
        "22023 CACHE (0) must be greater than zero"
    );
    assert_eq!(
        failure(
            &SequenceDeclaration {
                start: Some(Text("1.5".into())),
                ..SequenceDeclaration::default()
            },
            &ColumnType::BigInteger,
            false,
        ),
        "22P02 invalid input syntax for type bigint: \"1.5\""
    );
    assert_eq!(
        failure(
            &SequenceDeclaration {
                increment: Some(Integer(0)),
                max_value: Some(Text("3000000000".into())),
                ..SequenceDeclaration::default()
            },
            &ColumnType::Integer,
            false,
        ),
        "22023 INCREMENT must not be zero"
    );
}

#[test]
fn an_identity_sequence_reports_its_column_type() {
    assert_eq!(
        failure(&SequenceDeclaration::default(), &ColumnType::Text, true),
        "22023 identity column type must be smallint, integer, or bigint"
    );
    assert_eq!(
        failure(&SequenceDeclaration::default(), &ColumnType::Text, false),
        "22023 sequence type must be smallint, integer, or bigint"
    );
}
