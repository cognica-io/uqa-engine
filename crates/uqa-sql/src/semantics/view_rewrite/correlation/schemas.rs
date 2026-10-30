//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! DML analysis borrows its query arena and admits only private lexical bindings and row descriptors.

use super::{
    AutomaticViewLayer, ExpressionScope, InsertPlan, QueryPlan, ReturningAliases, RowSchema,
    SQLError, SourcePlan, ViewRewriteBindings, ViewRewriteContext,
};
use crate::{
    binding::production::{BindingNameSet, ControlledBindingContext},
    plan::{CtePlan, RetainedCtePlan},
};
use uqa_core::memory::ProductionControl;

pub(in crate::semantics::view_rewrite) fn analysis_scope<'a>(
    services: ViewRewriteContext<'_>,
    ctes: &[CtePlan],
    subqueries: &'a [QueryPlan],
    inherited: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<ControlledBindingContext<'a>, SQLError> {
    let base = services.catalog.binding_scope_with_control(control)?;
    let mut scope = ControlledBindingContext {
        catalog: base.catalog,
        resolution: base.resolution,
        ctes: base.ctes,
        deferred_ctes: base.deferred_ctes,
        non_returning_ctes: base.non_returning_ctes,
        scalar_subqueries: subqueries,
    };
    if let Some(parent) = inherited {
        parent.inherit(&mut scope, control)?;
    }
    for cte in ctes {
        control.check()?;
        scope.ctes.remove(&cte.name);
        if cte.body.returns_rows() {
            scope.non_returning_ctes.remove(&cte.name);
        } else {
            scope
                .non_returning_ctes
                .insert_with_control(&cte.name, control)?;
        }
        scope.deferred_ctes.insert_with_control(
            &cte.name,
            RetainedCtePlan::capture(cte, control)?,
            control,
        )?;
    }
    Ok(scope)
}

pub(in crate::semantics::view_rewrite) fn source_schema(
    services: ViewRewriteContext<'_>,
    source: Option<&SourcePlan>,
    ctes: &[CtePlan],
    subqueries: &[QueryPlan],
    params: &[crate::SQLParam],
    inherited: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    control.check()?;
    let Some(source) = source else {
        return Ok(None);
    };
    let scope = analysis_scope(services, ctes, subqueries, inherited, control)?;
    crate::binding::analyze_source_plan_schema_with_control(
        services.routines,
        source,
        params,
        scope,
        None,
        control,
    )
    .map(Some)
}

pub(in crate::semantics::view_rewrite) fn insert_input_width(
    services: ViewRewriteContext<'_>,
    plan: &InsertPlan,
    params: &[crate::SQLParam],
    inherited: Option<ViewRewriteBindings<'_>>,
    control: &ProductionControl<'_>,
) -> Result<usize, SQLError> {
    control.check()?;
    let Some(source) = plan.source.as_deref() else {
        return Ok(plan.rows.first().map_or(0, Vec::len));
    };
    let scope = analysis_scope(services, &plan.ctes, &plan.subqueries, inherited, control)?;
    let width = crate::binding::analyze_query_plan_schema_with_control(
        services.routines,
        source,
        params,
        scope,
        None,
        control,
    )?
    .len();
    plan.rule_values_input
        .as_ref()
        .map_or(Ok(width), |values| values.value_width(width))
}

pub(super) fn outer_schema(
    services: ViewRewriteContext<'_>,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    source: Option<&RowSchema>,
    aliases: Option<&ReturningAliases>,
    include_excluded: bool,
    control: &ProductionControl<'_>,
) -> Result<(RowSchema, BindingNameSet), SQLError> {
    let mut target = None;
    services.catalog.with_view_definition_with_control(
        &layer.canonical_name,
        control,
        &mut |definition| {
            let definition =
                definition.ok_or_else(|| SQLError::UnknownTable(layer.canonical_name.clone()))?;
            let schema = super::super::context::stored_view_schema_with_control(
                services, definition, control,
            )?;
            target = Some(RowSchema::with_column_metadata_with_control(
                Some(target_qualifier),
                schema.columns().iter().enumerate().map(|(position, name)| {
                    (
                        schema.public_name(position).unwrap_or(name),
                        schema.column_type(position),
                        None,
                    )
                }),
                control,
            )?);
            Ok(())
        },
    )?;
    let target = target.expect("view definition callback");
    let mut outer = target.clone();
    let mut qualifiers = BindingNameSet::new();
    qualifiers.insert_with_control(target_qualifier, control)?;
    if include_excluded {
        let excluded = RowSchema::with_column_metadata_with_control(
            Some("excluded"),
            target
                .columns()
                .iter()
                .enumerate()
                .map(|(position, name)| (name.as_str(), target.column_type(position), None)),
            control,
        )?;
        outer = RowSchema::join_with_control(&outer, &excluded, std::iter::empty(), control)?;
        qualifiers.insert_with_control("excluded", control)?;
    }
    if let Some(aliases) = aliases {
        let scope = ExpressionScope {
            target_qualifier,
            returning_aliases: Some(aliases),
            source,
            include_excluded: false,
        };
        for alias in [&aliases.old, &aliases.new] {
            if scope.row_image_qualifier(alias) {
                qualifiers.insert_with_control(alias, control)?;
            }
        }
        let columns = [&aliases.old, &aliases.new]
            .into_iter()
            .filter(|alias| scope.row_image_qualifier(alias))
            .flat_map(|alias| {
                target.columns().iter().enumerate().map(|(position, name)| {
                    (
                        Some(alias.as_str()),
                        name.as_str(),
                        target.column_type(position),
                    )
                })
            });
        outer = RowSchema::with_virtual_columns_with_control(&outer, columns, control)?;
    }
    if let Some(source) = source {
        outer = RowSchema::join_with_control(&outer, source, std::iter::empty(), control)?;
    }
    Ok((outer, qualifiers))
}
