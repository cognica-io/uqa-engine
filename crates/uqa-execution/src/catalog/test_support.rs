//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Minimal immutable catalog and routine fixtures for execution unit tests.

use super::{
    CatalogDefinitionSnapshot, CatalogReadSnapshot, CatalogReadView, CatalogTableSnapshot,
};
use std::{collections::BTreeMap, sync::Arc};
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
        )),
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
        tables: BTreeMap::default(),
        definitions: CatalogDefinitionSnapshot {
            builtin_routine_security: Arc::default(),
            foreign_wrappers: Arc::default(),
            foreign_servers: Arc::default(),
            sequence_persistence: Arc::default(),
            foreign_tables: Arc::default(),
            sql_user_functions: Arc::default(),
            role_memberships: Arc::default(),
            domains: Arc::default(),
            enums: Arc::default(),
            composites: Arc::default(),
            graphs: Arc::default(),
            views: Arc::default(),
            catalog_indexes: Arc::default(),
            database_security: crate::catalog::security::BoundDatabaseSecurity::bootstrap().into(),
            schemas: Arc::default(),
            sequences: Arc::default(),
            sequence_object_ids: Arc::default(),
            sequence_catalog_oids: Arc::default(),
            graph_catalog_oids: Arc::default(),
            sequence_security: Arc::default(),
            foreign_table_security: Arc::default(),
            system_relation_security: Arc::default(),
            roles: Arc::default(),
            triggers: Arc::default(),
            rules: Arc::default(),
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
