//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated DML validation and consuming rewrites share the same admitted namespaces.

pub(super) mod ids;
pub(super) mod owned;
pub(super) mod schemas;
pub(super) mod validation;

use super::{
    AutomaticViewLayer, BTreeSet, ConflictActionPlan, ConflictPlan, DeletePlan, ExpressionScope,
    InsertPlan, MergePlan, MergeWhenPlan, ProjectionPlan, QueryPlan, ReturningAliases, RowSchema,
    SQLError, ScalarExpr, SourcePlan, UpdatePlan, ViewRewriteBindings, ViewRewriteContext,
};
use uqa_core::memory::ProductionControl;

pub(super) fn schema_public_columns(schema: &RowSchema) -> BTreeSet<String> {
    schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| schema.public_name(position).unwrap_or(column).to_string())
        .collect()
}

pub(super) struct CorrelatedRewriteContext<'a> {
    pub(super) services: ViewRewriteContext<'a>,
    pub(super) layer: &'a AutomaticViewLayer,
    pub(super) default_target_qualifier: &'a str,
    pub(super) target_qualifiers: &'a crate::binding::production::BindingNameSet,
    pub(super) source: Option<&'a RowSchema>,
}

#[derive(Clone, Copy)]
pub(super) struct CorrelatedDmlContext<'a> {
    pub(super) services: ViewRewriteContext<'a>,
    pub(super) layer: &'a AutomaticViewLayer,
    pub(super) target_qualifier: &'a str,
    pub(super) source: Option<&'a RowSchema>,
    pub(super) returning_aliases: Option<&'a ReturningAliases>,
    pub(super) include_excluded: bool,
    pub(super) ctes: &'a [crate::plan::CtePlan],
    pub(super) inherited_ctes: Option<ViewRewriteBindings<'a>>,
    pub(super) ids: &'a ids::SubqueryIds,
    pub(super) params: &'a [crate::SQLParam],
}

pub(in crate::semantics::view_rewrite) fn rewrite(
    context: CorrelatedDmlContext<'_>,
    arena: &mut crate::plan::ScalarQueryArena,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let (_, qualifiers) = schemas::outer_schema(
        context.services,
        context.layer,
        context.target_qualifier,
        context.source,
        context.returning_aliases,
        context.include_excluded,
        control,
    )?;
    let rewrite = CorrelatedRewriteContext {
        services: context.services,
        layer: context.layer,
        default_target_qualifier: context.target_qualifier,
        target_qualifiers: &qualifiers,
        source: context.source,
    };
    for id in context.ids {
        control.check()?;
        if *id >= arena.queries().len() {
            return Err(SQLError::Internal(format!(
                "DML scalar subquery slot {id} is out of bounds"
            )));
        }
        arena.map_query(*id, control, |input| owned::query(&rewrite, input, control))?;
    }
    Ok(())
}
