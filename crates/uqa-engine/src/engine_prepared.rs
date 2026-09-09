//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, Engine, PreparedStatementPlan};

impl Engine {
    pub fn register_prepared(
        &self,
        name: String,
        definition: uqa_sql::ast::Statement,
    ) -> Result<(), uqa_sql::SQLError> {
        let plan = uqa_planner::UnifiedPlan::lower_with(definition, &|aggregate: &str| {
            self.has_registered_aggregate_function(aggregate)
        });
        self.register_prepared_plan(name, plan)
    }

    pub(crate) fn register_prepared_plan(
        &self,
        name: String,
        logical_plan: uqa_planner::UnifiedPlan,
    ) -> Result<(), uqa_sql::SQLError> {
        self.register_prepared_plan_with_types(name, logical_plan, &[])
    }

    pub(crate) fn register_prepared_plan_with_types(
        &self,
        name: String,
        mut logical_plan: uqa_planner::UnifiedPlan,
        declared: &[uqa_sql::ast::ColumnType],
    ) -> Result<(), uqa_sql::SQLError> {
        let mut parameter_types = declared
            .iter()
            .map(|ty| crate::sql::resolve_declared_column_type(self, ty).map(Some))
            .collect::<Result<Vec<_>, _>>()?;
        logical_plan.rewrite_scalar_expressions(&mut |expression| {
            if let uqa_execution::ScalarExpr::Param(index) = expression {
                parameter_types.resize(parameter_types.len().max(*index), None);
            }
        });
        let plan = crate::sql::optimize_engine_plan(self, logical_plan.clone())?;
        self.session.prepared.write().insert(
            name,
            PreparedStatementPlan {
                logical_plan: Arc::new(logical_plan),
                plan: Some(plan),
                parameter_types,
            },
        );
        Ok(())
    }

    pub(crate) fn prepared_parameter_types(
        &self,
        name: &str,
    ) -> Option<Vec<Option<uqa_sql::ast::ColumnType>>> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.parameter_types.clone())
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_planner::UnifiedPlan> {
        self.session
            .prepared
            .read()
            .get(name)
            .map(|entry| entry.plan.as_ref().unwrap_or(&entry.logical_plan).clone())
    }

    pub(crate) fn prepared_plan_for_execution(
        &self,
        name: &str,
    ) -> Result<Option<uqa_planner::UnifiedPlan>, uqa_sql::SQLError> {
        let Some(entry) = self.session.prepared.read().get(name).cloned() else {
            return Ok(None);
        };
        if let Some(plan) = entry.plan {
            return Ok(Some(plan));
        }
        let plan = crate::sql::optimize_engine_plan(self, (*entry.logical_plan).clone())?;
        if let Some(current) = self.session.prepared.write().get_mut(name) {
            if Arc::ptr_eq(&current.logical_plan, &entry.logical_plan) {
                current.plan = Some(plan.clone());
            }
        }
        Ok(Some(plan))
    }

    /// Invalidate executable plans without removing connection-owned definitions.
    /// Catalog changes are checked when each statement is next executed, so a
    /// dropped dependency cannot make unrelated commands or rollback fail.
    pub(crate) fn invalidate_prepared_plans(&self) {
        for prepared in self.session.prepared.write().values_mut() {
            prepared.plan = None;
        }
    }

    pub(crate) fn rebind_prepared_plans(&self) -> Result<(), uqa_sql::SQLError> {
        let plans = self
            .session
            .prepared
            .read()
            .iter()
            .map(|(name, prepared)| (name.clone(), prepared.logical_plan.clone()))
            .collect::<Vec<_>>();
        let mut rebound = Vec::with_capacity(plans.len());
        for (name, plan) in plans {
            rebound.push((
                name,
                crate::sql::optimize_engine_plan(self, (*plan).clone())?,
            ));
        }
        let mut prepared = self.session.prepared.write();
        for (name, plan) in rebound {
            if let Some(entry) = prepared.get_mut(&name) {
                entry.plan = Some(plan);
            }
        }
        Ok(())
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                self.session.prepared.write().remove(name);
            }
            None => self.session.prepared.write().clear(),
        }
    }
}
