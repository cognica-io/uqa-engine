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
        let mut parameter_types = declared
            .iter()
            .map(|ty| match ty {
                uqa_sql::ast::ColumnType::Named(name) if is_unknown_type(name)? => Ok(None),
                _ => crate::sql::resolve_declared_column_type(self, ty).map(Some),
            })
            .collect::<Result<Vec<_>, _>>()?;
        logical_plan.rewrite_scalar_expressions(&mut |expression| {
            if let uqa_execution::ScalarExpr::Param(index) = expression {
                parameter_types.resize(parameter_types.len().max(*index), None);
            }
        });
        let parameter_types =
            crate::sql::infer_prepared_parameter_types(self, &logical_plan, &parameter_types)?;
        let result_schema =
            crate::sql::analyze_prepared_plan(self, &logical_plan, &parameter_types)?;
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
        let mut custom = choose_custom_plan(&entry, &mode, generic_cost);
        if custom || generic_plan.is_none() {
            let result_schema = crate::sql::analyze_prepared_plan(
                self,
                &entry.logical_plan,
                &entry.parameter_types,
            )?;
            if !crate::sql::prepared_result_schema_matches(
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
            custom = choose_custom_plan(&entry, &mode, generic_cost);
        }
        let (plan, custom_cost) = if custom {
            let mut plan = (*entry.logical_plan).clone();
            specialize_parameters(&mut plan, parameters);
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

fn choose_custom_plan(
    entry: &PreparedStatementPlan,
    mode: &str,
    generic_cost: Option<f64>,
) -> bool {
    if entry.parameter_types.is_empty() {
        return false;
    }
    match mode {
        "force_generic_plan" => false,
        "force_custom_plan" => true,
        _ if entry.custom_plans < 5 => true,
        _ => generic_cost
            .is_some_and(|cost| cost >= entry.total_custom_cost / entry.custom_plans as f64),
    }
}

fn specialize_parameters(plan: &mut uqa_planner::UnifiedPlan, parameters: &[uqa_sql::SQLParam]) {
    use uqa_execution::ScalarExpr;
    use uqa_sql::SQLParam;
    plan.rewrite_scalar_expressions(&mut |expression| {
        let ScalarExpr::Param(index) = expression else {
            return;
        };
        let Some(parameter) = index.checked_sub(1).and_then(|index| parameters.get(index)) else {
            return;
        };
        *expression = match parameter {
            SQLParam::TypedScalar { value, ty } => ScalarExpr::TypedLiteral {
                value: value.clone(),
                ty: ty.sql_name(),
                bound_type: Some(ty.clone()),
                parameter_index: Some(*index),
            },
            SQLParam::Scalar(value) => ScalarExpr::Literal(value.clone()),
            SQLParam::Vector(_) | SQLParam::Tensor(_) => return,
        };
    });
}

fn is_unknown_type(name: &str) -> Result<bool, uqa_sql::SQLError> {
    Ok(uqa_sql::parse_regtype_name(name)?.is_some_and(|parsed| {
        parsed.array_dimensions == 0
            && !parsed.has_type_modifiers
            && match parsed.names.as_slice() {
                [local] => local == "unknown",
                [schema, local] => schema == "pg_catalog" && local == "unknown",
                _ => false,
            }
    }))
}
