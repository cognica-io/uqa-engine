//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog rows of standalone composite types: the composite relation in `pg_class`, the row type and its array type in `pg_type`, and the attributes in `pg_attribute`, where a dropped attribute keeps its number under `PostgreSQL`'s placeholder name.

use uqa_core::Value;
use uqa_sql::ast::ColumnType;
use uqa_sql::catalog::composite_type::StoredCompositeAttribute;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::CatalogReadView;

use super::super::helpers::oids::namespace_oid;
use super::super::helpers::rows::{bool_value, catalog_usize, int_value, str_value};
use super::attributes::{attribute_column, pg_attribute_row};
use super::pg_class_catalog_row;
use super::types::{pg_type_catalog_row, type_acl_value};

/// The `pg_type` rows of every composite type and its array type.
pub(super) fn composite_type_rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for definition in catalog.composites() {
        let ty = definition.column_type();
        let owner = int_value(definition.owner.oid);
        let namespace = namespace_oid(catalog, &definition.identity.schema);
        let mut entry = pg_type_catalog_row(&ty, namespace, "c", "C", false, 0, -1);
        entry.insert(
            "typrelid".into(),
            int_value(i64::from(definition.relation_oid)),
        );
        entry.insert("typowner".into(), owner.clone());
        entry.insert(
            "typacl".into(),
            type_acl_value(catalog, definition.usage_acl.as_deref())?,
        );
        rows.push(entry);
        let mut array = pg_type_catalog_row(
            &ColumnType::Array(Box::new(ty)),
            namespace,
            "b",
            "A",
            false,
            0,
            -1,
        );
        array.insert("typname".into(), str_value(definition.array_name.clone()));
        array.insert("typowner".into(), owner);
        rows.push(array);
    }
    Ok(rows)
}

/// The `pg_class` rows of every composite relation. `relnatts` counts dropped attributes, which keep their numbers.
pub fn composite_class_rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for definition in catalog.composites() {
        let mut row = pg_class_catalog_row(
            catalog,
            i64::from(definition.relation_oid),
            i64::from(definition.oid),
            &definition.identity.schema,
            &definition.identity.name,
            "c",
            catalog_usize(
                definition.attributes.len(),
                "pg_class composite attribute count",
            )?,
            -1.0,
            false,
        );
        row.insert("relowner".into(), int_value(definition.owner.oid));
        rows.push(row);
    }
    Ok(rows)
}

/// The `pg_attribute` rows of every composite relation.
pub(super) fn composite_attribute_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    for definition in catalog.composites() {
        for attribute in &definition.attributes {
            rows.push(composite_attribute_row(
                i64::from(definition.relation_oid),
                attribute,
            ));
        }
    }
    rows
}

fn composite_attribute_row(relid: i64, attribute: &StoredCompositeAttribute) -> ResultRow {
    let column = attribute_column(&attribute.name, attribute.ty.clone(), false);
    let mut row = pg_attribute_row(relid, i64::from(attribute.number), &column);
    if let Some(collation) = attribute
        .collation
        .as_deref()
        .and_then(uqa_sql::schema::composites::builtin_collation_oid)
    {
        row.insert("attcollation".into(), int_value(collation));
    }
    if attribute.dropped {
        row.insert(
            "attname".into(),
            str_value(StoredCompositeAttribute::dropped_name(attribute.number)),
        );
        row.insert("atttypid".into(), int_value(0));
        row.insert("attisdropped".into(), bool_value(true));
        row.insert("attstattarget".into(), Value::Null);
    }
    row
}
