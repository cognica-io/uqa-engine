//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Arc, Engine, PreparedStatementPlan};

#[cfg(test)]
mod tests;

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
        self.register_prepared_plan_with_types(name, logical_plan, &[], None)
    }

    pub(crate) fn register_prepared_plan_with_types(
        &self,
        name: String,
        mut logical_plan: uqa_planner::UnifiedPlan,
        declared: &[uqa_sql::ast::ColumnType],
        source_sql: Option<&str>,
    ) -> Result<(), uqa_sql::SQLError> {
        let parameter_types =
            uqa_sql::prepared::declared_parameter_types(self, &mut logical_plan, declared)?;
        let parameter_types =
            self.infer_prepared_parameter_types(&logical_plan, &parameter_types)?;
        let result_schema = self.analyze_prepared_plan(&logical_plan, &parameter_types)?;
        self.session.prepared.write().insert(
            name,
            PreparedStatementPlan {
                logical_plan: Arc::new(logical_plan),
                plan: None,
                parameter_types,
                result_schema,
                source_sql: source_sql.map(Arc::from),
                prepared_at_micros: uqa_sql::expr::clock_timestamp_micros(),
                from_sql: source_sql.is_some(),
                generic_plans: 0,
                custom_plans: 0,
                generic_cost: None,
                total_custom_cost: 0.0,
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
        parameters: &[uqa_sql::SQLParam],
    ) -> Result<Option<uqa_planner::UnifiedPlan>, uqa_sql::SQLError> {
        let Some(entry) = self.session.prepared.read().get(name).cloned() else {
            return Ok(None);
        };
        let mode = self.show_variable("plan_cache_mode")?;
        let mut generic_plan = entry.plan.clone();
        let mut generic_cost = entry.generic_cost;
        let usage = uqa_planner::statement_planning::prepared::PreparedPlanUsage {
            has_parameters: !entry.parameter_types.is_empty(),
            custom_plans: entry.custom_plans,
            total_custom_cost: entry.total_custom_cost,
        };
        let mut custom = uqa_planner::statement_planning::prepared::choose_custom_plan(
            usage,
            &mode,
            generic_cost,
        );
        if custom || generic_plan.is_none() {
            let result_schema =
                self.analyze_prepared_plan(&entry.logical_plan, &entry.parameter_types)?;
            if !uqa_sql::prepared::prepared_result_schema_matches(
                entry.result_schema.as_ref(),
                result_schema.as_ref(),
            ) {
                return Err(uqa_sql::SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: "cached plan must not change result type".into(),
                });
            }
        }
        if !custom && generic_plan.is_none() {
            let plan = crate::sql::optimize_engine_plan(self, (*entry.logical_plan).clone())?;
            generic_cost = Some(crate::sql::estimate_engine_plan(self, &plan)?.execution);
            generic_plan = Some(plan);
            // Building the first generic plan supplies its previously unknown cost.
            // Recheck before execution so an expensive generic plan is never used just to measure it.
            custom = uqa_planner::statement_planning::prepared::choose_custom_plan(
                usage,
                &mode,
                generic_cost,
            );
        }
        let (plan, custom_cost) = if custom {
            let mut plan = (*entry.logical_plan).clone();
            uqa_planner::statement_planning::prepared::specialize_parameters(&mut plan, parameters);
            let plan = crate::sql::optimize_engine_plan(self, plan)?;
            let cost = crate::sql::estimate_engine_plan(self, &plan)?
                .including_planning(&uqa_planner::CostEstimator::default());
            (plan, Some(cost))
        } else {
            (
                generic_plan
                    .as_ref()
                    .expect("generic plan was built")
                    .clone(),
                None,
            )
        };
        if let Some(current) = self.session.prepared.write().get_mut(name) {
            if Arc::ptr_eq(&current.logical_plan, &entry.logical_plan) {
                current.plan = generic_plan;
                current.generic_cost = generic_cost;
                if let Some(cost) = custom_cost {
                    current.total_custom_cost += cost;
                    current.custom_plans = current.custom_plans.saturating_add(1);
                } else {
                    current.generic_plans = current.generic_plans.saturating_add(1);
                }
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

    pub fn deallocate_prepared(&self, name: Option<&str>) {
        match name {
            Some(name) => {
                self.session.prepared.write().remove(name);
            }
            None => self.session.prepared.write().clear(),
        }
    }
}
