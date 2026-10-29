//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled binding inputs retain independent namespace and lexical-name owners while sharing immutable catalog, schema and executable generations.

use super::{BindingContext, SchemaScope};
use crate::{
    catalog::{analysis::CatalogReadView, resolution::RelationNameResolution},
    plan::{QueryPlan, RetainedCtePlan},
    RowSchema, SQLError,
};
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};

mod entries;
pub use entries::{
    analyze_expression_plan_type_with_control, analyze_query_plan_schema_with_control,
    analyze_source_plan_schema_with_control, bind_query_plan_schema_with_control,
};
mod names;
mod namespace;
pub use namespace::ControlledStoredQueryNamespace;
mod resolution;
pub use names::{BindingIter, BindingMap, BindingNameSet};
pub use resolution::BindingResolution;

pub struct ControlledBindingSnapshot {
    pub catalog: CatalogReadView,
    pub resolution: BindingResolution,
    pub ctes: BindingMap<RowSchema>,
    pub deferred_ctes: BindingMap<RetainedCtePlan>,
    pub non_returning_ctes: BindingNameSet,
    pub scalar_subqueries: Arc<[QueryPlan]>,
}

pub struct ControlledBindingContext<'a> {
    pub catalog: CatalogReadView,
    pub resolution: BindingResolution,
    pub ctes: BindingMap<RowSchema>,
    pub deferred_ctes: BindingMap<RetainedCtePlan>,
    pub non_returning_ctes: BindingNameSet,
    pub scalar_subqueries: &'a [QueryPlan],
}

impl ControlledBindingSnapshot {
    /// Capture borrowed lexical names; each supplied schema and retained executable already owns its immutable payload.
    pub fn from_inputs_with_control<'a>(
        catalog: CatalogReadView,
        resolution: Produced<RelationNameResolution>,
        ctes: impl IntoIterator<Item = (&'a str, RowSchema)>,
        deferred_ctes: impl IntoIterator<Item = (&'a str, &'a RetainedCtePlan)>,
        non_returning_ctes: impl IntoIterator<Item = &'a str>,
        scalar_subqueries: Arc<[QueryPlan]>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut output = Self {
            catalog,
            resolution: BindingResolution::from_produced(resolution),
            ctes: BindingMap::new(),
            deferred_ctes: BindingMap::new(),
            non_returning_ctes: BindingNameSet::new(),
            scalar_subqueries,
        };
        for (name, schema) in ctes {
            output.ctes.insert_with_control(name, schema, control)?;
        }
        for (name, plan) in deferred_ctes {
            output
                .deferred_ctes
                .insert_with_control(name, plan.clone(), control)?;
        }
        for name in non_returning_ctes {
            output
                .non_returning_ctes
                .insert_with_control(name, control)?;
        }
        control.check()?;
        Ok(output)
    }
    pub fn context_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'_>, SQLError> {
        control.check()?;
        Ok(ControlledBindingContext {
            catalog: self.catalog.clone(),
            resolution: self.resolution.copy_with_control(control)?,
            ctes: self
                .ctes
                .copy_with_control(control, |schema| Ok(schema.clone()))?,
            deferred_ctes: self
                .deferred_ctes
                .copy_with_control(control, |plan| Ok(plan.clone()))?,
            non_returning_ctes: self.non_returning_ctes.copy_with_control(control)?,
            scalar_subqueries: &self.scalar_subqueries,
        })
    }
}

impl super::snapshot::BindingSnapshot {
    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingSnapshot, SQLError> {
        ControlledBindingSnapshot::from_inputs_with_control(
            self.catalog.clone(),
            self.resolution.copy_with_control(control)?,
            self.ctes
                .iter()
                .map(|(name, schema)| (name.as_str(), schema.clone())),
            self.deferred_ctes
                .iter()
                .map(|(name, plan)| (name.as_str(), plan)),
            self.non_returning_ctes.iter().map(String::as_str),
            self.scalar_subqueries.clone(),
            control,
        )
    }
}

impl<'a> BindingContext<'a> {
    pub fn copy_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<ControlledBindingContext<'a>, SQLError> {
        control.check()?;
        let mut ctes = BindingMap::new();
        let mut deferred = BindingMap::new();
        let mut non_returning = BindingNameSet::new();
        let resolution =
            BindingResolution::from_produced(self.resolution.copy_with_control(control)?);
        for (name, schema) in &self.ctes {
            ctes.insert_with_control(name, schema.clone(), control)?;
        }
        for (name, plan) in &self.deferred_ctes {
            deferred.insert_with_control(name, plan.clone(), control)?;
        }
        for name in &self.non_returning_ctes {
            non_returning.insert_with_control(name, control)?;
        }
        Ok(ControlledBindingContext {
            catalog: self.catalog.clone(),
            resolution,
            ctes,
            deferred_ctes: deferred,
            non_returning_ctes: non_returning,
            scalar_subqueries: self.scalar_subqueries,
        })
    }
}

impl SchemaScope {
    pub(super) fn for_analysis_with_control(
        binding: ControlledBindingContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut scope = Self::from_context_with_control(binding, control)?;
        scope.validate_references = true;
        Ok(scope)
    }
    pub(super) fn from_context_with_control(
        binding: ControlledBindingContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let scope = Self {
            execution_sources: None,
            collations: super::preparation::PendingCollations::new(control),
            catalog: binding.catalog,
            resolution: binding.resolution,
            ctes: binding.ctes,
            deferred_ctes: binding.deferred_ctes,
            non_returning_ctes: binding.non_returning_ctes,
            production: control.capture(),
            visiting_views: std::collections::BTreeSet::new(),
            validate_references: false,
            stored_expression_outer: None,
            rule_condition_event: None,
            merge_returning: false,
            set_placement: crate::semantics::sets::placement::SetPlacement::Expanding,
            set_calls: 0,
            stored_whole_rows: std::collections::BTreeMap::new(),
            routine_parameters: None,
            scalar_binding: super::ScalarBindingMode::Stored,
            variable_sites: None,
            preserve_syntax_shape: false,
            prepared_dependencies: None,
            composite_dependencies: None,
        };
        scope.control().check()?;
        Ok(scope)
    }
    pub(super) fn control(&self) -> ProductionControl<'_> {
        self.production.control()
    }
}

impl ControlledBindingContext<'_> {
    pub fn copy_with_control(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        Ok(Self {
            catalog: self.catalog.clone(),
            resolution: self.resolution.copy_with_control(control)?,
            ctes: self
                .ctes
                .copy_with_control(control, |schema| Ok(schema.clone()))?,
            deferred_ctes: self
                .deferred_ctes
                .copy_with_control(control, |plan| Ok(plan.clone()))?,
            non_returning_ctes: self.non_returning_ctes.copy_with_control(control)?,
            scalar_subqueries: self.scalar_subqueries,
        })
    }
}
