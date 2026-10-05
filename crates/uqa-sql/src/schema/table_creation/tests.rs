//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, RowSchema};

struct Types;

impl FunctionTypeResolver for Types {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }

    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError> {
        if *ty == ColumnType::Text {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "type usage denied".into(),
            });
        }
        Ok(())
    }
}

#[test]
fn existing_create_as_targets_use_the_written_relation_name() {
    for name in ["existing", "private.existing"] {
        let error = existing_create_as_target(name, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42P07"));
        assert_eq!(error.to_string(), "relation \"existing\" already exists");
        let notice = existing_create_as_target(name, true).unwrap();
        assert_eq!(notice.sqlstate, "42P07");
        assert_eq!(
            notice.message,
            "relation \"existing\" already exists, skipping"
        );
    }
    let notice = existing_create_as_target("\"private\".\"mixed.name\"", true).unwrap();
    assert_eq!(
        notice.message,
        "relation \"mixed.name\" already exists, skipping"
    );
}

#[test]
fn query_defined_columns_apply_partial_names_before_duplicate_checks() {
    let schema = RowSchema::with_types(
        vec!["repeated".into(), "repeated".into()],
        vec![Some(ColumnType::Integer), Some(ColumnType::BigInteger)],
    );
    let columns = create_table_as_columns(&schema, &["first".into()]).unwrap();
    assert_eq!(columns[0].name, "first");
    assert_eq!(columns[1].name, "repeated");
    assert_eq!(columns[1].ty, ColumnType::BigInteger);
    validate_create_table_as_columns(&Types, &columns).unwrap();
    let error =
        create_table_as_columns(&schema, &["a".into(), "b".into(), "c".into()]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(error.to_string(), "too many column names were specified");
}

#[test]
fn query_defined_columns_check_duplicates_then_all_type_privileges_then_names() {
    let columns = [
        ColumnDef::nullable("xmin", ColumnType::Integer),
        ColumnDef::nullable("xmin", ColumnType::Text),
    ];
    let error = validate_create_table_as_columns(&Types, &columns).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42701"));
    assert_eq!(
        error.to_string(),
        "column \"xmin\" specified more than once"
    );
    let columns = [
        ColumnDef::nullable("xmin", ColumnType::Integer),
        ColumnDef::nullable("denied", ColumnType::Text),
    ];
    assert_eq!(
        validate_create_table_as_columns(&Types, &columns)
            .unwrap_err()
            .sqlstate(),
        Some("42501"),
    );
    assert_eq!(
        validate_create_table_as_columns(&Types, &columns[..1])
            .unwrap_err()
            .to_string(),
        "column name \"xmin\" conflicts with a system column name",
    );
}
