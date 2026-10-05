//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relations that own composite row types, described for type-dependency diagnostics.

use uqa_core::{catalog_role::RoleIdentity, RelationIdentity};
use uqa_sql::{ast::ColumnType, schema::domains::removal::RowTypeRelation, SQLError};

use crate::catalog::context::CatalogContext;
use crate::catalog::view::StoredViewKind;

/// Find the table, view, materialized view or foreign table whose row type has this OID. The relation name uses `regclass` output, which qualifies names hidden by the search path.
pub fn row_type_relation(
    context: &CatalogContext<'_>,
    oid: u32,
) -> Result<Option<RowTypeRelation>, SQLError> {
    let catalog = context.catalog_read_view();
    let oid = i64::from(oid);
    let describe = |kind: &'static str,
                    relation_oid: i64,
                    identity: &RelationIdentity,
                    owner: RoleIdentity|
     -> Result<Option<RowTypeRelation>, SQLError> {
        let name = super::resolve_regtype_output(context, &ColumnType::Regclass, relation_oid)
            .map_err(SQLError::Internal)?
            .unwrap_or_else(|| identity.qualified_name());
        Ok(Some(RowTypeRelation {
            kind,
            name,
            local_name: identity.name.clone(),
            owner,
            schema: identity.schema.clone(),
        }))
    };
    let snapshot = catalog.snapshot();
    for (identity, table) in &snapshot.tables {
        if i64::from(table.catalog_oids.reltype()) == oid {
            return describe(
                "table",
                i64::from(table.catalog_oids.relation),
                identity,
                table.security.role_owner,
            );
        }
    }
    for (identity, view) in snapshot.definitions.views.iter() {
        if super::super::view_rowtype_oid(view) == oid {
            let kind = match view.definition.kind {
                StoredViewKind::Materialized => "materialized view",
                StoredViewKind::View => "view",
            };
            return describe(
                kind,
                super::super::view_relation_oid(view),
                identity,
                view.security.role_owner,
            );
        }
    }
    for (identity, table) in snapshot.definitions.foreign_tables.iter() {
        if super::super::foreign_table_rowtype_oid(table) == oid {
            let owner = snapshot
                .definitions
                .foreign_table_security
                .get(identity)
                .map_or(RoleIdentity::BOOTSTRAP, |security| security.role_owner);
            return describe(
                "foreign table",
                super::super::foreign_table_relation_oid(table),
                identity,
                owner,
            );
        }
    }
    Ok(None)
}

/// Resolve any type object through the same catalog and visibility rules as ordinary `regtype` input.
pub fn resolve_type_object_oid(
    context: &CatalogContext<'_>,
    name: &str,
) -> Result<Option<i64>, SQLError> {
    super::resolve_regobject_oid(context, &ColumnType::Regtype, name)
}

/// `format_type_be` for any type object, including relation row types, which print as their relation's name.
pub fn format_type_object(
    context: &CatalogContext<'_>,
    oid: i64,
) -> Result<Option<String>, String> {
    super::resolve_regtype_output(context, &ColumnType::Regtype, oid)
}
