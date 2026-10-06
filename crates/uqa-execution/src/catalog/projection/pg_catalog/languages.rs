//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bootstrap language rows and their catalog relation descriptors.

use uqa_core::{catalog_role::RoleIdentity, RelationIdentity, Value};
use uqa_sql::{
    catalog::{
        languages::{language_name, C_LANGUAGE, INTERNAL_LANGUAGE, PLPGSQL_LANGUAGE, SQL_LANGUAGE},
        relation_oids::RelationCatalogOids,
        VirtualRelation,
    },
    ResultRow, SQLError,
};

use crate::catalog::CatalogReadView;

use super::super::helpers::rows::{bool_value, catalog_ordinal, int_value, row, str_value};
use super::attributes::{attribute_column, pg_attribute_row};
use super::relations::pg_class_catalog_row;

const LANGUAGE_ROW_TYPE: u32 = 10_021;
const LANGUAGE_ARRAY_TYPE: u32 = 10_020;

pub fn build_pg_language() -> Vec<ResultRow> {
    [
        (INTERNAL_LANGUAGE, false, false, 0, 0, 2246),
        (C_LANGUAGE, false, false, 0, 0, 2247),
        (SQL_LANGUAGE, false, true, 0, 0, 2248),
        (PLPGSQL_LANGUAGE, true, true, 13_644, 13_645, 13_646),
    ]
    .into_iter()
    .map(|(oid, procedural, trusted, call, inline, validator)| {
        row([
            ("oid", int_value(i64::from(oid))),
            (
                "lanname",
                str_value(language_name(oid).expect("bootstrap language identity")),
            ),
            ("lanowner", int_value(RoleIdentity::BOOTSTRAP.oid)),
            ("lanispl", bool_value(procedural)),
            ("lanpltrusted", bool_value(trusted)),
            ("lanplcallfoid", int_value(call)),
            ("laninline", int_value(inline)),
            ("lanvalidator", int_value(validator)),
            ("lanacl", Value::Null),
        ])
    })
    .collect()
}

pub(in crate::catalog::projection) fn language_class_row(catalog: &CatalogReadView) -> ResultRow {
    let relation = VirtualRelation::PgLanguage;
    pg_class_catalog_row(
        catalog,
        relation.oid(),
        i64::from(LANGUAGE_ROW_TYPE),
        relation.namespace(),
        relation.name(),
        "r",
        9,
        4.0,
        true,
    )
}

pub(super) fn language_attribute_rows() -> Result<Vec<ResultRow>, SQLError> {
    let relation = VirtualRelation::PgLanguage;
    relation
        .schema()
        .into_iter()
        .enumerate()
        .map(|(index, (name, ty))| {
            Ok(pg_attribute_row(
                relation.oid(),
                catalog_ordinal(index, "pg_language attribute")?,
                &attribute_column(&name, ty, name != "lanacl"),
            ))
        })
        .collect()
}

pub(super) fn language_type_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let relation = VirtualRelation::PgLanguage;
    let mut rows = Vec::new();
    super::row_types::append_rows(
        &mut rows,
        catalog,
        &RelationIdentity::new(relation.namespace(), relation.name()),
        RelationCatalogOids {
            relation: uqa_sql::catalog::dependencies::LANGUAGE_CLASS,
            row_type: Some(LANGUAGE_ROW_TYPE),
            array_type: Some(LANGUAGE_ARRAY_TYPE),
            rule: None,
        },
        RoleIdentity::BOOTSTRAP,
        Some("_pg_language"),
    );
    rows
}

#[cfg(test)]
mod tests;
