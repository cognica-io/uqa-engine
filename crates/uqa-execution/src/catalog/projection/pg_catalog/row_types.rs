//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The row type and generated array belonging to each retained relation definition.

use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{
    ast::{ColumnType, CompositeTypeReference},
    catalog::{array_type_names::array_type_name, relation_oids::RelationCatalogOids},
    ResultRow,
};

use crate::catalog::CatalogReadView;

use super::super::helpers::{
    oids::namespace_oid,
    rows::{int_value, str_value},
};
use super::types::pg_type_catalog_row;

#[cfg(test)]
mod tests;

/// Read only the immutable relation definitions: catalog inspection does not read table rows or execute stored view queries.
pub(super) fn relation_type_rows(catalog: &CatalogReadView) -> Vec<ResultRow> {
    let snapshot = catalog.snapshot();
    let mut rows = Vec::new();
    for (identity, table) in &snapshot.tables {
        append_rows(
            &mut rows,
            catalog,
            identity,
            table.catalog_oids,
            table.security.role_owner,
            table.row_type_array_name.as_deref(),
        );
    }
    for (identity, view) in snapshot.definitions.views.iter() {
        append_rows(
            &mut rows,
            catalog,
            identity,
            view.relation_oids(),
            view.security.role_owner,
            view.definition.row_type_array_name.as_deref(),
        );
    }
    for (identity, table) in snapshot.definitions.foreign_tables.iter() {
        let owner = snapshot
            .definitions
            .foreign_table_security
            .get(identity)
            .map_or(RoleIdentity::BOOTSTRAP, |security| security.role_owner);
        append_rows(
            &mut rows,
            catalog,
            identity,
            table.relation_oids(),
            owner,
            table.row_type_array_name.as_deref(),
        );
    }
    rows
}

fn append_rows(
    rows: &mut Vec<ResultRow>,
    catalog: &CatalogReadView,
    identity: &RelationIdentity,
    oids: RelationCatalogOids,
    owner: RoleIdentity,
    array_name: Option<&str>,
) {
    let Some(oid) = oids.row_type else {
        return;
    };
    let ty = ColumnType::Composite(CompositeTypeReference {
        schema: identity.schema.clone(),
        name: identity.name.clone(),
        oid,
        array_oid: oids.array_type.unwrap_or(0),
        relation_oid: oids.relation,
    });
    let namespace = namespace_oid(catalog, &identity.schema);
    let mut row = pg_type_catalog_row(&ty, namespace, "c", "C", false, 0, -1);
    row.insert("typrelid".into(), int_value(i64::from(oids.relation)));
    row.insert("typowner".into(), int_value(owner.oid));
    rows.push(row);
    if oids.array_type.is_some() {
        let mut array = pg_type_catalog_row(
            &ColumnType::Array(Box::new(ty)),
            namespace,
            "b",
            "A",
            false,
            0,
            -1,
        );
        array.insert(
            "typname".into(),
            str_value(array_name.map_or_else(|| array_type_name(&identity.name, 0), str::to_owned)),
        );
        array.insert("typowner".into(), int_value(owner.oid));
        rows.push(array);
    }
}
