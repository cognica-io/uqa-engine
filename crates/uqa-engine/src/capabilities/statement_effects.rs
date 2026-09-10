//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition adapters for SQL plan-effect classification.

use crate::Engine;
use uqa_sql::{
    ast::RelationPersistence,
    catalog::domain::StoredDomain,
    plan::{QueryPlan, UnifiedPlan},
    SQLError,
};

impl uqa_sql::semantics::effects::QueryEffectCatalog for Engine {
    fn registered_runtime_function_may_mutate_engine(&self, name: &str) -> bool {
        Engine::registered_runtime_function_may_mutate_engine(self, name)
    }
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        Engine::domain_by_oid(self, oid)
    }
    fn sequence_persistence(&self, name: &str) -> Result<Option<RelationPersistence>, String> {
        Engine::sequence_persistence(self, name).map_err(|error| error.to_string())
    }
    fn table_persistence(&self, name: &str) -> Result<Option<RelationPersistence>, String> {
        Engine::table_persistence(self, name).map_err(|error| error.to_string())
    }
    fn view_plan(&self, name: &str) -> Result<Option<QueryPlan>, SQLError> {
        Engine::view_plan(self, name)
    }
    fn lookup_prepared(&self, name: &str) -> Option<UnifiedPlan> {
        Engine::lookup_prepared(self, name)
    }
}

impl Engine {
    pub(crate) fn query_effect_context(
        &self,
    ) -> uqa_sql::semantics::effects::QueryEffectContext<'_> {
        uqa_sql::semantics::effects::QueryEffectContext {
            catalog: self,
            optimizer_effects: uqa_planner::optimizer::query_contains_implicit_hybrid_fusion,
            graph_effects: uqa_execution::query::graph_effects::query_is_mutating,
        }
    }
}
