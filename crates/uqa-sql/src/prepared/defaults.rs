//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A prepared mutation retains the domain defaults selected by query rewrite.

use crate::{
    ast::{ColumnDef, Expr},
    binding::production::BindingResolution,
    catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution},
    plan::{CommandPlan, MutableCommandPlan, OwnedScopedPlanVisitor, PlanRef, ScopedPlanVisitor},
    prepared::{dependencies::PreparedAnalysisDependencies, ownership::SharedPreparedInput},
    semantics::view_rewrite::{
        context::ViewRewriteContext,
        production::{automatic_view_layer_owned, OwnedAutomaticViewLayer},
    },
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
mod targets;

#[derive(Debug)]
pub struct DomainDefaultExpressions {
    pub table: String,
    pub columns: Vec<(String, Expr)>,
}

pub type PreparedDomainDefaults = Option<SharedPreparedInput<DomainDefaultExpressions>>;

/// `PostgreSQL` permits a data-modifying `WITH` item only at the top query level.
pub(crate) fn needs_domain_defaults(plan: PlanRef<'_>) -> bool {
    let ctes = match plan {
        PlanRef::Command(
            CommandPlan::Insert(_) | CommandPlan::Update(_) | CommandPlan::Merge(_),
        ) => return true,
        PlanRef::Command(command) => command.ctes(),
        PlanRef::Query(query) => &query.ctes,
    };
    ctes.iter().any(|cte| cte.body.modifies_data())
}

pub(crate) struct BindMutationDefaults<'a> {
    pub catalog: &'a dyn AnalysisCatalog,
    pub resolution: &'a RelationNameResolution,
    pub views: Option<ViewRewriteContext<'a>>,
    pub dependencies: &'a mut PreparedAnalysisDependencies,
    pub control: ProductionControl<'a>,
}

impl BindMutationDefaults<'_> {
    fn view_layers(
        &mut self,
        target: &str,
        lookup: &RelationNameResolution,
    ) -> Result<Option<Produced<Vec<OwnedAutomaticViewLayer>>>, SQLError> {
        let mut layers: ProductionVec<'_, OwnedAutomaticViewLayer> =
            ProductionVec::new(self.control);
        if let Some(views) = self.views {
            loop {
                let name = layers
                    .last()
                    .map_or(target, |layer| layer.source_name.as_str());
                let Some(layer) = automatic_view_layer_owned(views, name, &self.control)? else {
                    break;
                };
                if layers
                    .iter()
                    .any(|previous| *previous.canonical_name == *layer.canonical_name)
                {
                    return Ok(None);
                }
                if let Some(oid) = self.catalog.relation_dependency_with_control(
                    lookup,
                    &layer.source_name,
                    &self.control,
                )? {
                    self.dependencies
                        .include_relation_with_control(oid, &self.control)?;
                }
                // The layer retains its own field allocations. The vector owns its inline slot.
                layers.push_produced(
                    self.control
                        .finish(layer, self.control.empty_reservation())?,
                )?;
            }
        }
        Ok(Some(layers.finish()?))
    }

    fn bind(&mut self, command: &CommandPlan) -> Result<PreparedDomainDefaults, SQLError> {
        let (target, bound) = match command {
            CommandPlan::Insert(plan) => (&plan.table, plan.target_relation_bound),
            CommandPlan::Update(plan) => (&plan.table, plan.target_relation_bound),
            CommandPlan::Merge(plan) => (&plan.target, false),
            _ => return Ok(None),
        };
        let mut resolution;
        let lookup = if bound {
            resolution =
                BindingResolution::from_produced(self.resolution.copy_with_control(&self.control)?);
            resolution.set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound);
            &resolution
        } else {
            self.resolution
        };
        let Some(layers) = self.view_layers(target, lookup)? else {
            return Ok(None);
        };
        let physical = layers
            .last()
            .map_or(target.as_str(), |layer| layer.source_name.as_str());
        let output = crate::plan::copy::SyntaxProduction::new(self.control);
        let mut entries = output.vector(0)?;
        let mut collect = |columns: &[ColumnDef]| -> Result<(), SQLError> {
            if !columns
                .iter()
                .any(|col| matches!(col.ty, ColumnType::Domain { .. }))
            {
                return Ok(());
            }
            let selected = targets::TargetDefaults::new(command, &layers)?;
            for (index, column) in columns.iter().enumerate() {
                self.control.check()?;
                let ColumnType::Domain { oid, .. } = &column.ty else {
                    continue;
                };
                if column.default.is_some()
                    || column.generated.is_some()
                    || column.auto_increment.is_some()
                    || !selected.needs(&column.name, index)
                {
                    continue;
                }
                let value = if let Some(expression) = self
                    .catalog
                    .domain_default_with_control(*oid, &self.control)?
                {
                    let lowered = crate::plan::ExpressionPlan::lower_borrowed_for_rewrite(
                        expression,
                        &self.control,
                    )?;
                    self.dependencies
                        .include_expression_with_control(lowered.scalar(), &self.control)?;
                    output.copy(expression)?
                } else {
                    Expr::Literal(uqa_core::Value::Null)
                };
                entries.push((output.text(&column.name)?, value))?;
            }
            Ok(())
        };
        if let Some(table) =
            self.catalog
                .table_resolved_with_control(lookup, physical, &self.control)?
        {
            collect(&table.columns)?;
        } else {
            self.catalog.with_foreign_table_columns_with_control(
                lookup,
                physical,
                &self.control,
                &mut collect,
            )?;
        }
        let columns = entries.finish()?;
        if columns.is_empty() {
            return Ok(None);
        }
        let table = self
            .catalog
            .table_name_resolved_with_control(lookup, physical, &self.control)?
            .map_or_else(|| output.text(physical), |name| Ok(output.take(name)))?;
        SharedPreparedInput::from_produced(
            output.finish(DomainDefaultExpressions { table, columns })?,
            &self.control,
        )
        .map(Some)
    }
}

impl ScopedPlanVisitor for BindMutationDefaults<'_> {
    fn command(
        &mut self,
        command: &mut CommandPlan,
        _: &dyn Fn(&str) -> bool,
    ) -> Result<bool, SQLError> {
        let defaults = self.bind(command)?;
        command.set_prepared_domain_defaults(defaults);
        Ok(true)
    }
}

impl OwnedScopedPlanVisitor for BindMutationDefaults<'_> {
    fn command(
        &mut self,
        mut command: MutableCommandPlan,
        _: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableCommandPlan, bool), SQLError> {
        let defaults = self.bind(&command)?;
        command.set_prepared_domain_defaults(defaults, &self.control)?;
        Ok((command, true))
    }
}

impl CommandPlan {
    pub(crate) fn set_prepared_domain_defaults(&mut self, defaults: PreparedDomainDefaults) {
        match self {
            Self::Insert(plan) => plan.prepared_domain_defaults = defaults,
            Self::Update(plan) => plan.prepared_domain_defaults = defaults,
            Self::Merge(plan) => plan.prepared_domain_defaults = defaults,
            _ => {}
        }
    }
}
