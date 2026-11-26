//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project system catalog descriptors into their relation, attribute and type catalogs.

use super::super::helpers::rows::catalog_ordinal;
use super::{
    attributes::{attribute_column, pg_attribute_row},
    relations::pg_class_catalog_row,
};
use crate::catalog::CatalogReadView;
use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    catalog::{catalog_descriptors, relation_oids::RelationCatalogOids},
    ResultRow, SQLError,
};

pub fn class_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    catalog_descriptors()
        .map(|descriptor| {
            let relation = descriptor.relation;
            pg_class_catalog_row(
                catalog,
                relation.oid(),
                i64::from(descriptor.row_type),
                relation.namespace(),
                relation.name(),
                "r",
                i64::try_from(relation.schema().len()).expect("catalog column count"),
                0.0,
                true,
            )
        })
        .collect()
}

pub fn attribute_rows() -> Result<Vec<ResultRow>, SQLError> {
    let mut rows = Vec::new();
    for descriptor in catalog_descriptors() {
        for (index, (name, ty)) in descriptor.relation.schema().into_iter().enumerate() {
            rows.push(pg_attribute_row(
                descriptor.relation.oid(),
                catalog_ordinal(index, "catalog attribute")?,
                &attribute_column(&name, ty, index < descriptor.required_columns),
            ));
        }
    }
    Ok(rows)
}

pub fn type_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let mut rows = Vec::new();
    for descriptor in catalog_descriptors() {
        let relation = descriptor.relation;
        let array_name = format!("_{}", relation.name());
        super::row_types::append_rows(
            &mut rows,
            catalog,
            &RelationIdentity::new(relation.namespace(), relation.name()),
            RelationCatalogOids {
                relation: u32::try_from(relation.oid()).expect("catalog relation OID"),
                row_type: Some(descriptor.row_type),
                array_type: Some(descriptor.array_type),
                rule: None,
            },
            RoleIdentity::BOOTSTRAP,
            Some(&array_name),
        );
    }
    rows
}
