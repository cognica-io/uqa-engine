//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::context::{ScopedSubqueryHooks, SubqueryProbe, SubqueryQueryContexts};
use super::{SubqueryContext, SubqueryServices};
use crate::catalog::{
    services::{CatalogSession, CatalogSnapshotSource},
    CatalogDefinitionSnapshot, CatalogReadSnapshot, CatalogReadView, RelationLookupMode,
    RelationNameResolution,
};
use crate::query::{
    runtime::QueryMemorySettings, scope::subqueries::CachedScalarSubquery, sources::SourceContext,
    statement::context::QueryContext, CteScope,
};
use crate::scalar::plan::{PhysicalOuterRow, PhysicalSubqueryRunner};
use crate::{Batch, PhysicalRow, RowSchema, SpillBuffer, SubqueryResult};
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::Value;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility},
    catalog::session::PreparedStatementMetadata,
    expr::EngineHook,
    plan::{QueryPlan, UnifiedPlan},
    semantics::volatility::VolatilityCatalog,
    SQLError, SQLParam,
};

#[derive(Default)]
pub(super) struct Services {
    pub events: Mutex<Vec<&'static str>>,
    pub allow_metadata: bool,
    pub fail_memory: bool,
}

impl Services {
    pub fn services(&self) -> SubqueryServices<'_, ()> {
        SubqueryServices {
            catalog: self,
            session: self,
            volatility: self,
            queries: self,
            hooks: self,
        }
    }

    pub fn context<'a>(&'a self, scope: &'a CteScope) -> SubqueryContext<'a, ()> {
        SubqueryContext {
            services: self.services(),
            memory: self,
            ctes: scope,
            function_hook: self,
            subquery_runner: self,
        }
    }
}

impl QueryMemorySettings for Services {
    fn work_mem_bytes(&self) -> Result<usize, SQLError> {
        self.events.lock().push("memory");
        if self.fail_memory {
            Err(SQLError::Internal("memory setting unavailable".into()))
        } else {
            Ok(1)
        }
    }
}

impl CatalogSnapshotSource for Services {
    fn catalog_snapshot(&self) -> CatalogReadView {
        assert!(
            self.allow_metadata,
            "cached execution must not capture metadata"
        );
        self.events.lock().push("catalog");
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

    fn refreshed_catalog_snapshot(&self) -> Result<CatalogReadView, SQLError> {
        panic!("subquery correlation does not refresh the catalog")
    }
}

impl CatalogSession for Services {
    fn relation_name_resolution(&self) -> RelationNameResolution {
        assert!(
            self.allow_metadata,
            "cached execution must not capture resolution"
        );
        self.events.lock().push("resolution");
        RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "owner".into(),
            lookup_mode: RelationLookupMode::Dynamic,
        }
    }
    fn current_user(&self) -> String {
        panic!("unexpected session read")
    }
    fn temporary_schema_name(&self) -> String {
        panic!("unexpected session read")
    }
    fn show_variable(&self, _: &str) -> Result<String, SQLError> {
        panic!("unexpected session read")
    }
    fn runtime_parameter_source(&self, _: &str) -> &'static str {
        panic!("unexpected session read")
    }
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata> {
        panic!("unexpected session read")
    }
}

impl VolatilityCatalog for Services {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        self.events.lock().push("volatility");
        Some(if name == "volatile_value" {
            FunctionVolatility::Volatile
        } else {
            FunctionVolatility::Immutable
        })
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}

impl SubqueryQueryContexts<()> for Services {
    fn query_context(&self) -> QueryContext<'_, ()> {
        panic!("cached execution must not construct a query context")
    }
    fn source_context(&self) -> SourceContext<'_, ()> {
        panic!("execution without an outer row must not construct a source context")
    }
}

impl ScopedSubqueryHooks<()> for Services {
    fn with_hooks(&self, scope: &CteScope, probe: SubqueryProbe<'_>) -> Result<bool, SQLError> {
        self.events.lock().push("hooks");
        assert_eq!(scope.scalar_subqueries.len(), 1);
        probe(self, self)
    }
}

impl EngineHook for Services {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value, SQLError>> {
        assert_eq!(name, "key_value");
        self.events.lock().push("key");
        Some(Ok(Value::Int(7)))
    }
}

impl PhysicalSubqueryRunner for Services {
    fn execute_subquery(
        &self,
        _: usize,
        _: &QueryPlan,
        _: PhysicalOuterRow<'_>,
        _: &[SQLParam],
    ) -> Result<SubqueryResult, SQLError> {
        self.events.lock().push("nested");
        Ok(SubqueryResult::from_rows(
            vec!["value".into()],
            vec![[("value".into(), Value::Int(7))].into()],
        ))
    }
}

pub(super) fn plan(sql: &str) -> QueryPlan {
    let UnifiedPlan::Query(plan) = UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("expected query")
    };
    *plan
}

pub(super) fn materialized(values: Vec<Value>, budget: usize) -> CachedScalarSubquery {
    let schema = RowSchema::new(vec!["value".into()]);
    let mut spill = SpillBuffer::new(budget);
    if !values.is_empty() {
        spill
            .push(Batch::from_physical_rows(
                schema.clone(),
                values
                    .into_iter()
                    .map(|value| PhysicalRow::from_values(vec![value]))
                    .collect(),
            ))
            .unwrap();
    }
    CachedScalarSubquery {
        columns: vec!["value".into()],
        rows: spill.into_shared(schema).unwrap(),
    }
}
