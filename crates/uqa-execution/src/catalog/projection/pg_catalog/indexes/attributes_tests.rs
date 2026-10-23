//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::{ColumnDef, ColumnType, Expr, IndexKey};

fn index(keys: Vec<IndexKey>, definition: IndexDefinition) -> CatalogIndexRelation {
    CatalogIndexRelation {
        relation: RelationIdentity::new("public", "document_idx"),
        table_name: "public.documents".into(),
        index_type: "btree".into(),
        columns: keys,
        definition,
        primary: false,
        relkind: "i",
        is_partition: false,
        has_children: false,
        parent_index_oid: None,
    }
}

fn attributes(
    index: &CatalogIndexRelation,
    columns: &[ColumnDef],
    columns_declared: bool,
) -> Vec<(String, ColumnType)> {
    index
        .attributes(columns, columns_declared)
        .unwrap()
        .into_iter()
        .map(|column| (column.name, column.ty))
        .collect()
}

#[test]
fn document_fields_keep_text_catalog_types_without_stored_key_types() {
    let index = index(
        vec![IndexKey::Column("owner_id".into())],
        IndexDefinition {
            included_columns: vec!["extra".into()],
            ..IndexDefinition::default()
        },
    );
    assert_eq!(
        attributes(&index, &[], false),
        vec![
            ("owner_id".into(), ColumnType::Text),
            ("extra".into(), ColumnType::Text),
        ]
    );
}

#[test]
fn stored_index_types_and_names_take_precedence_over_document_defaults() {
    let index = index(
        vec![IndexKey::Column("renamed".into())],
        IndexDefinition {
            key_names: vec!["original".into()],
            key_types: vec![ColumnType::BigInteger],
            ..IndexDefinition::default()
        },
    );
    assert_eq!(
        attributes(&index, &[], false),
        vec![("original".into(), ColumnType::BigInteger)]
    );
}

#[test]
fn declared_columns_supply_legacy_index_types_and_reject_missing_fields() {
    let mut index = index(
        vec![IndexKey::Column("owner_id".into())],
        IndexDefinition::default(),
    );
    let columns = vec![ColumnDef::nullable("owner_id", ColumnType::Integer)];
    assert_eq!(
        attributes(&index, &columns, true),
        vec![("owner_id".into(), ColumnType::Integer)]
    );
    assert!(index
        .attributes(&[ColumnDef::nullable("other", ColumnType::Text)], true)
        .is_err());
    index.definition.included_columns.push("missing".into());
    assert!(matches!(
        index.attributes(&columns, true),
        Err(SQLError::UnknownColumn(_))
    ));
}

#[test]
fn expression_keys_still_require_their_analyzed_type() {
    let mut index = index(
        vec![IndexKey::Expression(Box::new(Expr::Literal(Value::Int(1))))],
        IndexDefinition::default(),
    );
    assert!(matches!(
        index.attributes(&[], false),
        Err(SQLError::Internal(_))
    ));
    index.definition.key_types.push(ColumnType::Integer);
    assert_eq!(
        attributes(&index, &[], false),
        vec![("expr".into(), ColumnType::Integer)]
    );
}

#[test]
fn declared_zero_column_tables_do_not_supply_document_field_types() {
    let mut index = index(
        vec![IndexKey::Column("missing".into())],
        IndexDefinition::default(),
    );
    assert!(matches!(
        index.attributes(&[], true),
        Err(SQLError::Internal(_))
    ));
    index.definition.key_types.push(ColumnType::Text);
    index.definition.included_columns.push("extra".into());
    assert!(matches!(
        index.attributes(&[], true),
        Err(SQLError::UnknownColumn(name)) if name == "extra"
    ));
}
