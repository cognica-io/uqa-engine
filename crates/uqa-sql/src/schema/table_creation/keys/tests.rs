//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::declare_primary_key_not_null;
use crate::ast::{ColumnDef, ColumnType, TableKeyConstraint, TableKeyConstraintKind};
use crate::schema::keys::definition::index_order;

fn key(kind: TableKeyConstraintKind, name: Option<&str>, columns: &[&str]) -> TableKeyConstraint {
    TableKeyConstraint {
        catalog_identity: None,
        index_identity: None,
        name: name.map(str::to_owned),
        kind,
        columns: columns.iter().map(|column| (*column).to_owned()).collect(),
        included_columns: Vec::new(),
        nulls_not_distinct: false,
        without_overlaps: false,
    }
}

fn column(name: &str) -> ColumnDef {
    let Some(crate::ast::Statement::CreateTable(table)) =
        crate::compiler::compile(&format!("CREATE TABLE t ({name} integer)"))
            .unwrap()
            .into_iter()
            .next()
    else {
        panic!("CREATE TABLE");
    };
    let mut column = table.columns.into_iter().next().expect("column");
    column.ty = ColumnType::Integer;
    column
}

#[test]
fn the_primary_key_leads_and_repeated_indexes_give_their_names_to_unnamed_keys() {
    use TableKeyConstraintKind::{PrimaryKey, Unique};
    let ordered = index_order(vec![
        key(Unique, Some("u1"), &["a"]),
        key(Unique, None, &["b"]),
        key(PrimaryKey, None, &["a"]),
        key(Unique, Some("u2"), &["b"]),
        key(Unique, Some("u3"), &["b"]),
    ]);
    assert_eq!(
        ordered
            .iter()
            .map(|key| (key.kind, key.name.as_deref(), key.columns.clone()))
            .collect::<Vec<_>>(),
        [
            (PrimaryKey, Some("u1"), vec!["a".to_owned()]),
            (Unique, Some("u2"), vec!["b".to_owned()]),
        ]
    );
    let mut overlaps = key(Unique, None, &["a", "b"]);
    overlaps.without_overlaps = true;
    let mut nulls = key(Unique, None, &["a"]);
    nulls.nulls_not_distinct = true;
    assert_eq!(
        index_order(vec![
            overlaps,
            key(Unique, None, &["a", "b"]),
            nulls,
            key(Unique, None, &["a"]),
        ])
        .len(),
        4
    );
}

#[test]
fn a_declared_primary_key_makes_an_inherited_not_null_local() {
    let mut declared = column("a");
    let mut explicit = column("b");
    explicit.not_null = true;
    explicit.not_null_explicit = true;
    explicit.not_null_name = Some("named".into());
    let mut inherited = column("c");
    inherited.not_null = true;
    inherited.not_null_is_local = false;
    inherited.not_null_name = Some("parent_c_not_null".into());
    declared.not_null_is_local = true;
    let mut columns = vec![declared, explicit, inherited, column("d")];
    declare_primary_key_not_null(
        &mut columns,
        &[key(
            TableKeyConstraintKind::PrimaryKey,
            None,
            &["a", "b", "c"],
        )],
    );
    assert_eq!(
        columns
            .iter()
            .map(|column| (
                column.not_null,
                column.not_null_is_local,
                column.not_null_name.as_deref()
            ))
            .collect::<Vec<_>>(),
        [
            (true, true, None),
            (true, true, Some("named")),
            (true, true, None),
            (false, true, None),
        ]
    );
}
