//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared range and multirange types retain their own names and arrays while sharing the range's owner.

use super::{
    int_value, namespace_oid, pg_type_catalog_row, row, str_value, type_acl_value, CatalogReadView,
    ColumnType,
};
use uqa_sql::{ResultRow, SQLError};

pub(super) fn type_rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for definition in catalog.ranges() {
        for (ty, identity, array_name, kind, acl) in [
            (
                definition.column_type(),
                &definition.identity,
                &definition.array_name,
                "r",
                definition.usage_acl.as_deref(),
            ),
            (
                definition.multirange_type(),
                &definition.multirange.identity,
                &definition.multirange.array_name,
                "m",
                None,
            ),
        ] {
            let namespace = namespace_oid(catalog, &identity.schema);
            let mut entry = pg_type_catalog_row(&ty, namespace, kind, "R", false, 0, -1);
            entry.insert("typowner".into(), int_value(definition.owner.oid));
            entry.insert("typacl".into(), type_acl_value(catalog, acl)?);
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
            array.insert("typname".into(), str_value(array_name.clone()));
            array.insert("typowner".into(), int_value(definition.owner.oid));
            rows.push(array);
        }
    }
    Ok(rows)
}

/// Keep the existing native projection and append definition-derived support identities without reselecting them from endpoint types.
pub fn build_pg_range_with_catalog(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let mut rows = super::build_pg_range();
    rows.extend(catalog.ranges().map(|definition| {
        row([
            ("rngtypid", int_value(i64::from(definition.oid))),
            (
                "rngsubtype",
                int_value(super::pg_type_oid(&definition.subtype)),
            ),
            (
                "rngmultitypid",
                int_value(i64::from(definition.multirange.oid)),
            ),
            (
                "rngcollation",
                int_value(i64::from(definition.collation_oid)),
            ),
            (
                "rngsubopc",
                int_value(i64::from(definition.subtype_operator_class)),
            ),
            (
                "rngcanonical",
                int_value(i64::from(definition.canonical_oid.unwrap_or(0))),
            ),
            (
                "rngsubdiff",
                int_value(i64::from(definition.subtype_difference_oid.unwrap_or(0))),
            ),
        ])
    }));
    rows
}
