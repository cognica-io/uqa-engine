//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Minimal immutable catalog and routine fixtures for execution unit tests.

use super::{
    CatalogDefinitionSnapshot, CatalogReadSnapshot, CatalogReadView, CatalogTableSnapshot,
};
use std::sync::Arc;
use uqa_sql::{ast::FunctionBinding, ColumnType, SQLError};

mod services;
pub(crate) use services::CatalogServices;

pub(crate) fn table_snapshot(
    object_id: [u8; 16],
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
) -> CatalogTableSnapshot {
    CatalogTableSnapshot {
        dropped_attributes: constraints.dropped_attributes.into(),
        object_id,
        catalog_oids: constraints.catalog_oids.unwrap_or_else(|| {
            uqa_sql::catalog::relation_oids::RelationCatalogOids::legacy(
                uqa_sql::catalog::relation_oids::RelationOidKind::Table,
                &object_id,
            )
        }),
        row_type_array_name: constraints.row_type_array_name,
        security: Arc::new(super::security::BoundTableSecurity::owner(
            uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
        ))
        .into(),
        columns: columns.into(),
        columns_declared: true,
        checks: constraints.checks.into(),
        foreign_keys: constraints.foreign_keys.into(),
        keys: constraints.key_constraints.into(),
        hierarchy: constraints.hierarchy.into(),
        persistence: constraints.persistence,
    }
}

pub(crate) fn empty_catalog() -> CatalogReadView {
    CatalogReadView::new(CatalogReadSnapshot {
        tables: uqa_core::memory::OwnedMap::default(),
        graph_overlay: None,
        definitions: CatalogDefinitionSnapshot {
            builtin_routine_security: uqa_core::memory::RetainedValue::default(),
            foreign_wrappers: uqa_core::memory::RetainedValue::default(),
            foreign_servers: uqa_core::memory::RetainedValue::default(),
            foreign_tables: Arc::default(),
            sql_user_functions: uqa_core::memory::RetainedValue::default(),
            role_memberships: uqa_core::memory::RetainedValue::default(),
            domains: uqa_core::memory::RetainedValue::default(),
            enums: uqa_core::memory::RetainedValue::default(),
            ranges: uqa_core::memory::RetainedValue::default(),
            shells: uqa_core::memory::RetainedValue::default(),
            comments: uqa_core::memory::RetainedValue::default(),
            composites: uqa_core::memory::RetainedValue::default(),
            graphs: uqa_core::memory::RetainedValue::default(),
            views: uqa_core::memory::RetainedValue::default(),
            catalog_indexes: uqa_core::memory::RetainedValue::default(),
            database_security: uqa_core::memory::RetainedValue::new(
                uqa_sql::catalog::security::database::DatabaseAuthority::bootstrap(),
            ),
            schemas: uqa_core::memory::RetainedValue::default(),
            sequences: uqa_core::memory::RetainedValue::default(),
            graph_catalog_oids: uqa_core::memory::RetainedValue::default(),
            foreign_table_security: Arc::default(),
            system_relation_security: uqa_core::memory::RetainedValue::default(),
            roles: uqa_core::memory::RetainedValue::default(),
            triggers: uqa_core::memory::RetainedValue::default(),
            rules: uqa_core::memory::RetainedValue::default(),
        },
        temporary_namespace: None,
    })
}

pub(crate) struct NoRoutines;

impl uqa_sql::FunctionTypeResolver for NoRoutines {
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
}
impl uqa_sql::routines::RoutineResolution for NoRoutines {}

impl uqa_sql::semantics::volatility::VolatilityCatalog for NoRoutines {
    fn host_function_volatility(&self, _: &str) -> Option<uqa_sql::ast::FunctionVolatility> {
        None
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<uqa_sql::ast::FunctionVolatility>> {
        None
    }

    fn view_query(&self, _: &str) -> Result<Option<uqa_sql::plan::QueryPlan>, SQLError> {
        Ok(None)
    }
}

pub(crate) fn empty_scope() -> crate::query::CteScope {
    crate::query::CteScope::with_catalog(
        empty_catalog(),
        uqa_sql::catalog::resolution::RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "owner".into(),
            lookup_mode: uqa_sql::catalog::resolution::RelationLookupMode::Dynamic,
        },
        None,
    )
}

/// Projection fixtures use the complete native relation publisher.
pub(crate) fn foreign_tables(
    values: impl IntoIterator<
        Item = (
            uqa_core::RelationIdentity,
            super::foreign::StoredForeignTable,
        ),
    >,
) -> std::sync::Arc<super::foreign::ForeignTables> {
    let mut catalog = super::foreign::ForeignCatalog::default();
    for (relation, table) in values {
        catalog
            .insert(
                relation,
                table,
                uqa_sql::catalog::security::BoundTableSecurity::owner(
                    uqa_sql::catalog::roles::RoleIdentity::BOOTSTRAP,
                ),
            )
            .unwrap();
    }
    catalog.tables_snapshot()
}
pub(crate) fn index_rows(
    rows: impl IntoIterator<Item = uqa_storage::CatalogIndexRow>,
) -> crate::catalog::index::registry::CatalogIndexes {
    let mut indexes = crate::catalog::index::registry::CatalogIndexes::default();
    for row in rows {
        indexes
            .insert(row, uqa_sql::ast::RelationPersistence::Permanent)
            .unwrap();
    }
    indexes
}

/// Mutate a fixture through the same complete admitted authority owner as execution.
pub(crate) fn edit_sequence_security(
    snapshot: &mut super::sequence::snapshot::SequenceReadSnapshot,
    edit: impl FnOnce(&mut uqa_sql::catalog::security::BoundSequenceSecurity),
) {
    let (relation, mut security) = snapshot
        .catalog
        .security()
        .iter()
        .next()
        .map(|(relation, security)| (relation.clone(), security.clone()))
        .unwrap();
    edit(&mut security);
    uqa_core::memory::RetainedValue::make_mut(&mut snapshot.catalog)
        .replace_security(&relation, security)
        .unwrap();
}
