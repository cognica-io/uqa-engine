//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::validate_type_target;
use crate::ast::{
    BinaryOp, ColumnDef, ColumnType, Expr, GeneratedColumn, GeneratedColumnKind, PartitionSpec,
    PartitionStrategy,
};
use crate::SQLError;
use uqa_core::Value;

fn partition(key: Expr) -> PartitionSpec {
    PartitionSpec {
        strategy: PartitionStrategy::List,
        keys: vec![key],
    }
}

fn assert_diagnostic(error: &SQLError, state: &str, message: &str, detail: Option<&str>) {
    assert_eq!(error.sqlstate(), Some(state));
    assert_eq!(error.to_string(), message);
    assert_eq!(error.detail(), detail);
    assert_eq!(error.hint(), None);
}

// Diagnostic text and order follow the PostgreSQL 18.4 ALTER COLUMN TYPE capture, including missing/system targets, inherited and expression partition keys, and both generated-column kinds.
#[test]
fn missing_and_system_columns_precede_hierarchy_checks() {
    let columns = [ColumnDef::nullable("a", ColumnType::Integer)];
    let key = partition(Expr::Column("missing".into()));
    assert_diagnostic(
        &validate_type_target(
            "typecheck_parent",
            &columns,
            "missing",
            true,
            true,
            Some(&key),
        )
        .unwrap_err(),
        "42703",
        "column \"missing\" of relation \"typecheck_parent\" does not exist",
        None,
    );
    for name in crate::schema::columns::POSTGRES_SYSTEM_COLUMNS {
        assert_diagnostic(
            &validate_type_target("typecheck_parent", &columns, name, true, true, Some(&key))
                .unwrap_err(),
            "0A000",
            &format!("cannot alter system column \"{name}\""),
            None,
        );
    }
}

#[test]
fn generated_using_precedes_inheritance_and_partition_checks() {
    for (name, kind) in [
        ("stored", GeneratedColumnKind::Stored),
        ("virtual", GeneratedColumnKind::Virtual),
    ] {
        let mut column = ColumnDef::nullable(name, ColumnType::Integer);
        column.generated = Some(GeneratedColumn {
            kind,
            expression: Box::new(Expr::Literal(Value::Int(1))),
            function_dependencies: Vec::new(),
        });
        let columns = [column];
        let key = partition(Expr::Column(name.into()));
        assert_diagnostic(
            &validate_type_target(
                "typecheck_generated",
                &columns,
                name,
                true,
                true,
                Some(&key),
            )
            .unwrap_err(),
            "42611",
            "cannot specify USING when altering type of generated column",
            Some(&format!("Column \"{name}\" is a generated column.")),
        );
        let target =
            validate_type_target("typecheck_generated", &columns, name, false, false, None)
                .unwrap();
        assert!(std::ptr::eq(target, &columns[0]));
    }
}

#[test]
fn inherited_column_precedes_partition_key_rejection() {
    let columns = [ColumnDef::nullable("a", ColumnType::Integer)];
    let key = partition(Expr::Column("a".into()));
    for has_using in [false, true] {
        assert_diagnostic(
            &validate_type_target(
                "typecheck_child",
                &columns,
                "a",
                has_using,
                true,
                Some(&key),
            )
            .unwrap_err(),
            "42P16",
            "cannot alter inherited column \"a\"",
            None,
        );
    }
}

#[test]
fn direct_and_expression_partition_keys_reject_each_referenced_column() {
    let columns = [
        ColumnDef::nullable("a", ColumnType::Integer),
        ColumnDef::nullable("b", ColumnType::Integer),
        ColumnDef::nullable("local", ColumnType::Integer),
    ];
    let direct = partition(Expr::Column("a".into()));
    assert_diagnostic(
        &validate_type_target("typecheck_partitioned", &columns, "a", false, false, Some(&direct))
            .unwrap_err(),
        "42P16",
        "cannot alter column \"a\" because it is part of the partition key of relation \"typecheck_partitioned\"",
        None,
    );
    let expression = partition(Expr::Binary {
        op: BinaryOp::Add,
        lhs: Box::new(Expr::Column("a".into())),
        rhs: Box::new(Expr::QualifiedColumn {
            qualifier: "typecheck_expression".into(),
            column: "b".into(),
        }),
    });
    for name in ["a", "b"] {
        assert_diagnostic(
            &validate_type_target("typecheck_expression", &columns, name, true, false, Some(&expression))
                .unwrap_err(),
            "42P16",
            &format!("cannot alter column \"{name}\" because it is part of the partition key of relation \"typecheck_expression\""),
            None,
        );
    }
    let target = validate_type_target(
        "typecheck_expression",
        &columns,
        "local",
        true,
        false,
        Some(&expression),
    )
    .unwrap();
    assert!(std::ptr::eq(target, &columns[2]));
}

#[test]
fn partition_diagnostic_preserves_quoted_local_relation_and_column_names() {
    let table = crate::RelationIdentity::new("odd.schema", "A.b").qualified_name();
    let columns = [ColumnDef::nullable("Has Space", ColumnType::Integer)];
    let key = partition(Expr::Column("Has Space".into()));
    assert_diagnostic(
        &validate_type_target(&table, &columns, "Has Space", false, false, Some(&key)).unwrap_err(),
        "42P16",
        "cannot alter column \"Has Space\" because it is part of the partition key of relation \"A.b\"",
        None,
    );
}
