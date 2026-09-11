//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Minimal immutable catalog and routine fixtures for execution unit tests.

use super::{CatalogDefinitionSnapshot, CatalogReadSnapshot, CatalogReadView};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{ast::FunctionBinding, ColumnType, SQLError};

pub(crate) fn empty_catalog() -> CatalogReadView {
    CatalogReadView::new(CatalogReadSnapshot {
        tables: BTreeMap::default(),
        definitions: CatalogDefinitionSnapshot {
            sequence_persistence: Arc::default(),
            foreign_tables: Arc::default(),
            sql_user_functions: Arc::default(),
            role_memberships: Arc::default(),
            domains: Arc::default(),
            graphs: Arc::default(),
            views: Arc::default(),
            catalog_indexes: Arc::default(),
            database_security: crate::catalog::security::DatabaseSecurity::bootstrap().into(),
            schemas: Arc::default(),
            sequences: Arc::default(),
            sequence_object_ids: Arc::default(),
            sequence_security: Arc::default(),
            foreign_table_security: Arc::default(),
            roles: Arc::default(),
            triggers: Arc::default(),
            rules: Arc::default(),
        },
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
