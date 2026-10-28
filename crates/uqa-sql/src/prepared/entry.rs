//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared plan identity, cached variants and catalog-visible metadata.

use std::sync::Arc;

#[derive(Clone)]
pub struct PreparedStatementPlan {
    pub composite_inputs: super::composites::CompositeInputs,
    /// Original input syntax, retained for parse analysis after invalidation.
    pub source_plan: Arc<crate::plan::UnifiedPlan>,
    pub logical_plan: Arc<crate::plan::UnifiedPlan>,
    pub needs_analysis: bool,
    /// The effective namespace of the retained analysis, when its catalog supplies one.
    pub effective_search_path: Option<crate::catalog::resolution::EffectiveSearchPath>,
    pub dependencies: super::dependencies::PreparedAnalysisDependencies,
    pub dependency_snapshot: Option<super::dependencies::PreparedDependencySnapshot>,
    /// Immutable generic executable shared by the cache and its active executions.
    pub plan: Option<Arc<crate::plan::UnifiedPlan>>,
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
    /// Publish successful descriptor reanalysis without counting a plan execution.
    pub fn record_analysis(&mut self, definition: super::definition::PreparedDefinition) {
        self.composite_inputs = definition.composite_inputs;
        self.logical_plan = Arc::new(definition.logical_plan);
        self.effective_search_path = definition.effective_search_path;
        self.dependencies = definition.dependencies;
        self.dependency_snapshot = definition.dependency_snapshot;
        self.needs_analysis = false;
        self.plan = None;
        self.generic_cost = None;
    }

    pub fn invalidate(&mut self) {
        if !super::utility::requires_analysis(&self.source_plan) {
            return;
        }
        self.plan = None;
        self.needs_analysis = true;
    }

    pub fn record_execution(&mut self, update: super::planning::PreparedPlanUpdate) {
        self.composite_inputs = update.composite_inputs;
        if let Some(analysis) = update.reanalyzed {
            self.logical_plan = analysis.logical_plan;
            self.effective_search_path = analysis.effective_search_path;
            self.dependencies = analysis.dependencies;
            self.dependency_snapshot = analysis.dependency_snapshot;
            self.needs_analysis = false;
        }
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

mod reuse;
