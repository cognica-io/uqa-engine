//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared plan identity, cached variants and catalog-visible metadata.

use std::sync::Arc;

#[derive(Clone)]
pub struct PreparedStatementPlan {
    pub logical_plan: Arc<crate::plan::UnifiedPlan>,
    pub plan: Option<crate::plan::UnifiedPlan>,
    pub parameter_types: Vec<Option<crate::ast::ColumnType>>,
    pub result_schema: Option<crate::RowSchema>,
    pub source_sql: Option<Arc<str>>,
    pub prepared_at_micros: i64,
    pub from_sql: bool,
    pub generic_plans: i64,
    pub custom_plans: i64,
    pub generic_cost: Option<f64>,
    pub total_custom_cost: f64,
}

use crate::catalog::session::PreparedStatementMetadata;

impl PreparedStatementPlan {
    pub fn metadata(&self, name: &str) -> PreparedStatementMetadata {
        PreparedStatementMetadata {
            name: name.to_string(),
            parameter_types: self.parameter_types.clone(),
            result_types: self
                .result_schema
                .as_ref()
                .map(|schema| schema.column_types().to_vec()),
            source_sql: self.source_sql.clone(),
            prepared_at_micros: self.prepared_at_micros,
            from_sql: self.from_sql,
            generic_plans: self.generic_plans,
            custom_plans: self.custom_plans,
        }
    }
}

impl PreparedStatementPlan {
    pub fn record_execution(&mut self, update: super::planning::PreparedPlanUpdate) {
        self.plan = update.generic_plan;
        self.generic_cost = update.generic_cost;
        if let Some(cost) = update.custom_cost {
            self.total_custom_cost += cost;
            self.custom_plans = self.custom_plans.saturating_add(1);
        } else {
            self.generic_plans = self.generic_plans.saturating_add(1);
        }
    }
}
