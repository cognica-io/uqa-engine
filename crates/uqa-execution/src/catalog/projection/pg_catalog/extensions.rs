//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata for the bundled PL/pgSQL extension and its catalog relation.

use uqa_core::{catalog_role::RoleIdentity, RelationIdentity, Value};
use uqa_sql::{
    catalog::{
        dependencies::EXTENSION_CLASS,
        languages::{language_name, PLPGSQL_EXTENSION, PLPGSQL_LANGUAGE},
        relation_oids::RelationCatalogOids,
        VirtualRelation,
    },
    ResultRow, SQLError,
};

use crate::catalog::CatalogReadView;

use super::super::helpers::{
    oids::namespace_oid,
    rows::{bool_value, catalog_ordinal, int_value, row, str_value},
};
use super::attributes::{attribute_column, pg_attribute_row};
use super::relations::pg_class_catalog_row;

const EXTENSION_ROW_TYPE: u32 = 10_074;
const EXTENSION_ARRAY_TYPE: u32 = 10_073;

pub fn build_pg_extension(catalog: &CatalogReadView) -> Vec<ResultRow> {
    vec![row([
        ("oid", int_value(i64::from(PLPGSQL_EXTENSION))),
        (
            "extname",
            str_value(language_name(PLPGSQL_LANGUAGE).expect("bundled language identity")),
        ),
        ("extowner", int_value(RoleIdentity::BOOTSTRAP.oid)),
        (
            "extnamespace",
            int_value(namespace_oid(catalog, "pg_catalog")),
        ),
        ("extrelocatable", bool_value(false)),
        ("extversion", str_value("1.0")),
        ("extconfig", Value::Null),
        ("extcondition", Value::Null),
    ])]
}

pub(in crate::catalog::projection) fn extension_class_row(catalog: &CatalogReadView) -> ResultRow {
    let relation = VirtualRelation::PgExtension;
    pg_class_catalog_row(
        catalog,
        relation.oid(),
        i64::from(EXTENSION_ROW_TYPE),
        relation.namespace(),
        relation.name(),
        "r",
        8,
        1.0,
        true,
    )
}

pub(super) fn extension_attribute_rows() -> Result<Vec<ResultRow>, SQLError> {
    let relation = VirtualRelation::PgExtension;
    relation
        .schema()
        .into_iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            Ok(pg_attribute_row(
                relation.oid(),
                catalog_ordinal(index, "pg_extension attribute")?,
                &attribute_column(
                    &name,
                    ty,
                    !matches!(name.as_str(), "extconfig" | "extcondition"),
                ),
            ))
        })
        .collect()
}

pub(super) fn type_oids() -> [i64; 2] {
    [
        i64::from(EXTENSION_ROW_TYPE),
        i64::from(EXTENSION_ARRAY_TYPE),
    ]
}

pub(super) fn extension_type_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let relation = VirtualRelation::PgExtension;
    let mut rows = Vec::new();
    super::row_types::append_rows(
        &mut rows,
        catalog,
        &RelationIdentity::new(relation.namespace(), relation.name()),
        RelationCatalogOids {
            relation: EXTENSION_CLASS,
            row_type: Some(EXTENSION_ROW_TYPE),
            array_type: Some(EXTENSION_ARRAY_TYPE),
            rule: None,
        },
        RoleIdentity::BOOTSTRAP,
        Some("_pg_extension"),
    );
    rows
}

#[cfg(test)]
mod tests;
