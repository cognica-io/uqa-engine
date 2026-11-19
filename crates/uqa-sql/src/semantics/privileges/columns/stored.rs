//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable dependencies share column lineage while retaining every declared CTE and whole-row identity.

use super::super::{
    analyze_query_plan, collect_expression_and_subqueries, context::AnalysisPurpose,
    lineage::relation_lineage, ownership::Columns, CteScope, QueryPlan, Required, SourceLineage,
};
use super::ColumnDependencies;
use crate::{
    plan::{CtePlan, CtePlanBody, PlanRef, ProjectionPlan},
    SQLError, ScalarExpr,
};

mod mutations;

/// Project base-column identities from an already bound action. The event row is visible only through OLD/NEW; local sources and action RETURNING aliases retain their own lexical scopes.
pub fn stored_plan(
    plan: PlanRef<'_>,
    event_relation: &str,
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let mut scope = scope.reborrow()?;
    scope.purpose = AnalysisPurpose::StoredDependencies;
    scope
        .resolution
        .set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound);
    let event = event_scope(event_relation, &scope)?;
    let mut required = Required::new();
    analyze(
        plan,
        &scope,
        &[&event],
        &mut SourceLineage::default(),
        &mut required,
    )?;
    super::by_relation(required, &scope)
}

/// Analyze captured source syntax without binding constants, functions or row identities again.
pub fn stored_expression(
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    event_relation: &str,
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<ColumnDependencies, SQLError> {
    let mut scope = scope.reborrow()?;
    scope.purpose = AnalysisPurpose::StoredDependencies;
    scope
        .resolution
        .set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound);
    let event = event_scope(event_relation, &scope)?;
    let mut required = Required::new();
    collect_expression_and_subqueries(
        expression,
        &[&event],
        false,
        subqueries,
        &scope,
        &mut SourceLineage::default(),
        &mut required,
    )?;
    super::by_relation(required, &scope)
}

fn event_scope(
    name: &str,
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<SourceLineage, SQLError> {
    let control = scope.control();
    let relation = scope
        .catalog
        .relation_with_control(&scope.resolution, name, &control)?
        .ok_or_else(|| SQLError::UnknownTable(name.into()))?;
    let mut event = relation_lineage(&relation, ["old", "new"], &[], &control)?;
    event.output = Columns::default();
    event.event_row = true;
    Ok(event)
}

pub(in crate::semantics::privileges) fn analyze(
    plan: PlanRef<'_>,
    scope: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    scope.control().check()?;
    match plan {
        PlanRef::Query(query) => analyze_query_plan(query, scope, outer, universe, required),
        PlanRef::Command(command) => mutations::analyze(command, scope, outer, universe, required),
    }
}

pub(in crate::semantics::privileges) fn ctes<'a>(
    declarations: &'a [CtePlan],
    scope: &'a CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<CteScope<'a, &'a [QueryPlan]>, SQLError> {
    let mut visible = scope.reborrow()?;
    let recursive = declarations.iter().any(|cte| cte.recursive);
    if recursive {
        for cte in declarations {
            visible.insert_deferred_ref(cte)?;
        }
    }
    for cte in declarations {
        scope.control().check()?;
        analyze(
            match &cte.body {
                CtePlanBody::Query(query) => PlanRef::Query(query),
                CtePlanBody::Command(command) => PlanRef::Command(command),
            },
            &visible,
            outer,
            universe,
            required,
        )?;
        if let Some(cycle) = &cte.cycle {
            for expression in [&cycle.mark_value, &cycle.mark_default] {
                collect_expression_and_subqueries(
                    expression,
                    outer,
                    false,
                    &[],
                    &visible,
                    universe,
                    required,
                )?;
            }
        }
        if !recursive {
            visible.insert_deferred_ref(cte)?;
        }
    }
    Ok(visible)
}

pub(in crate::semantics::privileges) fn is_output_alias(
    expression: &ScalarExpr,
    projections: &[ProjectionPlan],
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let ScalarExpr::Column(name) = expression else {
        return Ok(false);
    };
    for projection in projections {
        if crate::semantics::projection::with_projection_label(projection, control, |label| {
            Ok(label == name.as_str())
        })? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::semantics::privileges) fn cte_columns(
    cte: &CtePlan,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Columns, SQLError> {
    use super::super::ownership::OutputColumn;
    use uqa_core::memory::ProductionVec;
    let mut output = ProductionVec::new(*control);
    let mut position = 0;
    let mut push = |name: &str| {
        let name = cte.columns.get(position).map_or(name, String::as_str);
        position += 1;
        output.push_produced(control.finish(
            OutputColumn {
                name: control.copy_text(name)?,
                sources: Required::new(),
            },
            control.empty_reservation(),
        )?)?;
        Ok::<_, SQLError>(())
    };
    match &cte.body {
        CtePlanBody::Query(query) => {
            crate::semantics::projection::visit_query_output_columns(
                query,
                &[],
                control,
                &mut push,
            )?;
        }
        CtePlanBody::Command(command) => {
            for projection in command.returning().unwrap_or_default() {
                crate::semantics::projection::with_projection_label(
                    projection, control, &mut push,
                )?;
            }
        }
    }
    for name in cte
        .search
        .iter()
        .map(|search| search.sequence_column.as_str())
        .chain(
            cte.cycle
                .iter()
                .flat_map(|cycle| [cycle.mark_column.as_str(), cycle.path_column.as_str()]),
        )
    {
        output.push_produced(control.finish(
            OutputColumn {
                name: control.copy_text(name)?,
                sources: Required::new(),
            },
            control.empty_reservation(),
        )?)?;
    }
    Ok(Columns::from_produced(output.finish()?))
}
