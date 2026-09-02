//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, PreparedStatementPlan};

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
        let plan = crate::sql::optimize_engine_plan(self, logical_plan.clone())?;
        self.session
            .state
            .write()
            .prepared
            .insert(name, PreparedStatementPlan { logical_plan, plan });
        Ok(())
    }

    pub fn lookup_prepared(&self, name: &str) -> Option<uqa_planner::UnifiedPlan> {
        self.session
            .state
            .read()
            .prepared
            .get(name)
            .map(|entry| entry.plan.clone())
    }

    pub(crate) fn rebind_prepared_plans(&self) -> Result<(), uqa_sql::SQLError> {
        let plans = self
            .session
            .state
            .read()
            .prepared
            .iter()
            .map(|(name, prepared)| (name.clone(), prepared.logical_plan.clone()))
            .collect::<Vec<_>>();
        let mut rebound = Vec::with_capacity(plans.len());
        for (name, plan) in plans {
            rebound.push((name, crate::sql::optimize_engine_plan(self, plan)?));
        }
        let mut session = self.session.state.write();
        for (name, plan) in rebound {
            if let Some(entry) = session.prepared.get_mut(&name) {
                entry.plan = plan;
            }
        }
        Ok(())
    }

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                self.session.state.write().prepared.remove(name);
            }
            None => self.session.state.write().prepared.clear(),
        }
    }
}
