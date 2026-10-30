//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Embedding a view expression admits its private query namespace and every lexical replacement before construction.

use super::{qualifiers, renaming, scalars};
use crate::{
    binding::production::{BindingNameSet, ControlledBindingContext, ControlledBindingSnapshot},
    plan::{
        MutableExecutablePlan, MutableExpressionPlan, MutableQueryPlan, MutableSourcePlan,
        OwnedScalarExpr, QueryChildSlot, RelationalPlan, RetainedCtePlan, ScalarQueryArena,
        SourceChildSlot, SourcePlan, SourceScalarSlot,
    },
    semantics::view_rewrite::{source_qualifier_matches, AutomaticViewLayer, ViewRewriteContext},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

pub(in crate::semantics::view_rewrite) fn embed(
    services: ViewRewriteContext<'_>,
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<MutableExpressionPlan, SQLError> {
    produce(
        services.routines,
        expression,
        layer,
        target_qualifier,
        control,
        || services.catalog.binding_scope_with_control(control),
    )
}

fn produce(
    routines: &dyn crate::semantics::sets::SetFunctionCatalog,
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    control: &ProductionControl<'_>,
    binding_scope: impl FnOnce() -> Result<ControlledBindingSnapshot, SQLError>,
) -> Result<MutableExpressionPlan, SQLError> {
    let expression = expression
        .copy_for_rewrite(control)?
        .replace_nodes(control, &mut |node| {
            column(node, layer, target_qualifier, None, control)
        })?;
    let mut maximum = None;
    expression.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } => {
                maximum = Some(maximum.map_or(*id, |current: usize| current.max(*id)));
            }
            _ => {}
        }
        Ok::<_, SQLError>(true)
    })?;
    let Some(maximum) = maximum else {
        return MutableExpressionPlan::from_parts(
            expression,
            ScalarQueryArena::copy_from(&[], control)?,
            control,
        );
    };
    if maximum >= layer.subqueries.len() {
        return Err(SQLError::Internal(format!(
            "view `{}` expression has an out-of-bounds scalar subquery slot",
            layer.canonical_name,
        )));
    }
    let mut arena = ScalarQueryArena::copy_from(&layer.subqueries, control)?;
    let scope = binding_scope()?;
    let context = Context {
        routines,
        layer,
        target_qualifier,
        control,
    };
    let shadows = Shadows::default();
    for index in 0..arena.queries().len() {
        let qualifier = qualifiers::fresh(&arena.queries()[index], control)?;
        arena.map_query(index, control, |input| {
            let input = renaming::query(input, target_qualifier, &qualifier, false, control)?;
            context.query(input, scope.context_with_control(control)?, &shadows)
        })?;
    }
    MutableExpressionPlan::from_parts(expression, arena, control)
}

#[derive(Default)]
struct Shadows {
    qualifiers: BindingNameSet,
    columns: BindingNameSet,
}

fn column(
    node: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target: &str,
    shadows: Option<&Shadows>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let column = match node {
        ScalarExpr::Column(column)
            if shadows.is_none_or(|shadows| {
                !shadows.columns.contains(column)
                    && layer.source_schema.has_unqualified_column(column)
            }) =>
        {
            column
        }
        ScalarExpr::QualifiedColumn { qualifier, column }
            if shadows.is_none_or(|shadows| !shadows.qualifiers.contains(qualifier))
                && source_qualifier_matches(
                    qualifier,
                    &layer.source_qualifier,
                    &layer.source_name,
                ) =>
        {
            column
        }
        _ => return Ok(None),
    };
    Ok(Some(OwnedScalarExpr::positioned_column(
        control.copy_text(layer.physical_source_column(column))?,
        Some(control.copy_text(target)?),
        column.position(),
        control,
    )?))
}

struct Context<'a, 'c> {
    routines: &'a dyn crate::semantics::sets::SetFunctionCatalog,
    layer: &'a AutomaticViewLayer,
    target_qualifier: &'a str,
    control: &'a ProductionControl<'c>,
}

impl Context<'_, '_> {
    fn scalar(
        &self,
        input: OwnedScalarExpr,
        shadows: &Shadows,
    ) -> Result<OwnedScalarExpr, SQLError> {
        input.replace_nodes(self.control, &mut |node| {
            column(
                node,
                self.layer,
                self.target_qualifier,
                Some(shadows),
                self.control,
            )
        })
    }

    fn source(
        &self,
        mut input: MutableSourcePlan,
        scope: &ControlledBindingContext<'_>,
        shadows: &Shadows,
    ) -> Result<MutableSourcePlan, SQLError> {
        self.control.check()?;
        match &*input {
            SourcePlan::Join { .. } => {
                for slot in [SourceChildSlot::Left, SourceChildSlot::Right] {
                    input = input.map_child(slot, self.control, |input| {
                        self.source(input, scope, shadows)
                    })?;
                }
                input.map_scalar(SourceScalarSlot::JoinOn, self.control, |input| {
                    self.scalar(input, shadows)
                })
            }
            SourcePlan::Subquery { .. } => input.map_query(self.control, |input| {
                self.query(input, scope.copy_with_control(self.control)?, shadows)
            }),
            _ => scalars::source(input, self.control, &mut |input| {
                self.scalar(input, shadows)
            }),
        }
    }

    fn query(
        &self,
        mut input: MutableQueryPlan,
        mut scope: ControlledBindingContext<'_>,
        inherited: &Shadows,
    ) -> Result<MutableQueryPlan, SQLError> {
        let control = self.control;
        control.check()?;
        for cte in &input.ctes {
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
        for index in 0..input.ctes.len() {
            input = input.map_cte(index, control, |input| match input {
                MutableExecutablePlan::Query(input) => Ok(MutableExecutablePlan::Query(
                    self.query(input, scope.copy_with_control(control)?, inherited)?,
                )),
                MutableExecutablePlan::Command(_) => Err(SQLError::Unsupported(
                    "WITH clause containing a data-modifying statement must be at the top level"
                        .into(),
                )),
            })?;
        }
        let local = match &input.root {
            RelationalPlan::QueryBlock(block) => {
                let schema = block
                    .from
                    .as_ref()
                    .map(|source| {
                        crate::binding::production::analyze_source_plan_schema_with_control(
                            self.routines,
                            source,
                            &[],
                            scope.copy_with_control(control)?,
                            Some(&self.layer.source_schema),
                            control,
                        )
                    })
                    .transpose()?;
                let mut shadows = Shadows {
                    qualifiers: inherited.qualifiers.copy_with_control(control)?,
                    columns: inherited.columns.copy_with_control(control)?,
                };
                if let Some(schema) = schema {
                    let display =
                        crate::RelationIdentity::borrowed_reference(&self.layer.source_name)
                            .map(|reference| control.format(format_args!("{}", reference.name)))
                            .transpose()?;
                    for qualifier in [
                        self.layer.source_qualifier.as_str(),
                        self.layer.source_name.as_str(),
                    ]
                    .into_iter()
                    .chain(display.as_ref().map(|name| name.as_str()))
                    {
                        if schema.has_qualifier(qualifier) {
                            shadows.qualifiers.insert_with_control(qualifier, control)?;
                        }
                    }
                    for (position, name) in schema.columns().iter().enumerate() {
                        shadows.columns.insert_with_control(
                            schema.public_name(position).unwrap_or(name),
                            control,
                        )?;
                    }
                }
                input = input.map_source(control, |input| self.source(input, &scope, &shadows))?;
                Some(shadows)
            }
            RelationalPlan::SetOp { .. } => {
                for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                    input = input.map_query_child(slot, control, |input| {
                        self.query(input, scope.copy_with_control(control)?, inherited)
                    })?;
                }
                None
            }
            RelationalPlan::Values { .. } => None,
        };
        let shadows = local.as_ref().unwrap_or(inherited);
        input = scalars::query(input, scalars::Order::Rewrite, control, &mut |input| {
            self.scalar(input, shadows)
        })?;
        for index in 0..scalars::query_count(&input) {
            input = input.map_query_child(QueryChildSlot::Subquery(index), control, |input| {
                self.query(input, scope.copy_with_control(control)?, shadows)
            })?;
        }
        Ok(input)
    }
}

#[cfg(test)]
mod tests;
