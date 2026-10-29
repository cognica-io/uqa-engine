//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query volatility preserves lexical CTE scopes while each active view owns its admitted cycle-detection entry.

use super::{
    expression_contains_volatile_with_control, function_volatility_with_binding_and_control,
    scalar_calls_are_volatile_with_control,
};
use crate::{
    ast::FunctionVolatility,
    plan::{CtePlanBody, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan, UnifiedPlan},
    semantics::{functions::lowercase_function_name_with_control, volatility::VolatilityCatalog},
    SQLError, ScalarExpr,
};
use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::{
    memory::{OwnedMap, Produced, ProductionControl},
    RelationIdentity,
};

struct ViewName(Produced<String>);
impl Borrow<str> for ViewName {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}
impl PartialEq for ViewName {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}
impl Eq for ViewName {}
impl PartialOrd for ViewName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ViewName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_str().cmp(other.0.as_str())
    }
}

#[derive(Default)]
struct Views(OwnedMap<ViewName, ()>);
impl Views {
    fn enter(&mut self, name: &str, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
        control.check()?;
        if self.0.contains_key(name) {
            return Ok(false);
        }
        let name = ViewName(control.copy_text(name)?);
        if let Some(budget) = control.budget() {
            self.0.insert_budgeted(name, (), budget)?;
        } else {
            self.0.insert(name, ());
        }
        Ok(true)
    }
    fn leave(&mut self, name: &str) {
        self.0.remove(name);
    }
}

pub fn scalar_in_scope_contains_volatile_function_with_control(
    catalog: &dyn VolatilityCatalog,
    scalar: &ScalarExpr,
    subqueries: &[QueryPlan],
    visible_ctes: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if scalar_calls_are_volatile_with_control(catalog, scalar, control)? {
        return Ok(true);
    }
    let mut result = Ok(false);
    scalar.visit(&mut |node| {
        if !matches!(result, Ok(false)) {
            return;
        }
        let index = match node {
            ScalarExpr::ScalarSubquery(index)
            | ScalarExpr::Exists {
                subquery: index, ..
            }
            | ScalarExpr::InSubquery {
                subquery: index, ..
            } => *index,
            _ => return,
        };
        result = subqueries.get(index).map_or(Ok(true), |query| {
            query_inner(catalog, query, &mut Views::default(), visible_ctes, control)
        });
    });
    result
}

pub fn query_contains_volatile_function_with_control(
    catalog: &dyn VolatilityCatalog,
    plan: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    query_inner(catalog, plan, &mut Views::default(), &|_| false, control)
}

pub fn select_contains_volatile_function_with_control(
    catalog: &dyn VolatilityCatalog,
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for expression in block
        .deferred_outer_rewrites
        .iter()
        .flat_map(crate::semantics::view_rewrite::DeferredOuterRewrite::expressions)
    {
        if expression_contains_volatile_with_control(catalog, &expression.scalar, true, control)? {
            return Ok(true);
        }
    }
    for expression in block
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(block.r#where.iter())
        .chain(block.group_by.iter())
        .chain(block.grouping_sets.iter().flatten())
        .chain(block.having.iter())
        .chain(block.order_by.iter().map(|order| &order.expr))
        .chain(block.limit.iter())
        .chain(block.offset.iter())
        .chain(block.distinct_on.iter())
    {
        if scalar_calls_are_volatile_with_control(catalog, expression, control)? {
            return Ok(true);
        }
    }
    control.check()?;
    Ok(false)
}

fn query_inner(
    catalog: &dyn VolatilityCatalog,
    plan: &QueryPlan,
    visiting: &mut Views,
    outer: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let scope = |name: &str| plan.ctes.iter().any(|cte| cte.name == name) || outer(name);
    let recursive = plan.ctes.iter().any(|cte| cte.recursive);
    for (index, cte) in plan.ctes.iter().enumerate() {
        control.check()?;
        let visible = if recursive {
            &plan.ctes[..]
        } else {
            &plan.ctes[..index]
        };
        let definition_scope =
            |name: &str| visible.iter().any(|cte| cte.name == name) || outer(name);
        if match &cte.body {
            CtePlanBody::Query(query) => {
                query_inner(catalog, query, visiting, &definition_scope, control)?
            }
            CtePlanBody::Command(_) => true,
        } {
            return Ok(true);
        }
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            if select_contains_volatile_function_with_control(catalog, block, control)? {
                return Ok(true);
            }
            for subquery in &block.subqueries {
                if query_inner(catalog, subquery, visiting, &scope, control)? {
                    return Ok(true);
                }
            }
            match &block.from {
                Some(source) => source_inner(catalog, source, visiting, &scope, control),
                None => Ok(false),
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            if query_inner(catalog, left, visiting, &scope, control)?
                || query_inner(catalog, right, visiting, &scope, control)?
            {
                return Ok(true);
            }
            for expression in order_by
                .iter()
                .map(|order| &order.expr)
                .chain(limit.as_deref())
                .chain(offset.as_deref())
            {
                if scalar_calls_are_volatile_with_control(catalog, expression, control)? {
                    return Ok(true);
                }
            }
            for subquery in subqueries {
                if query_inner(catalog, subquery, visiting, &scope, control)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter().flatten() {
                if scalar_calls_are_volatile_with_control(catalog, expression, control)? {
                    return Ok(true);
                }
            }
            for subquery in subqueries {
                if query_inner(catalog, subquery, visiting, &scope, control)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }
}

fn source_inner(
    catalog: &dyn VolatilityCatalog,
    source: &SourcePlan,
    visiting: &mut Views,
    scope: &dyn Fn(&str) -> bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match source {
        SourcePlan::RetainedRows(_) => Ok(false),
        SourcePlan::Table { name, .. } => {
            if let Some(reference) = RelationIdentity::borrowed_reference(name)
                .filter(|reference| reference.schema.is_none())
            {
                let decoded = control.format(format_args!("{}", reference.name))?;
                if scope(&decoded) {
                    return Ok(false);
                }
            }
            let key = lowercase_function_name_with_control(name, control)?;
            if !visiting.enter(&key, control)? {
                return Ok(false);
            }
            let result = catalog
                .with_view_query_with_control(name, control, &mut |view| {
                    query_inner(catalog, view, visiting, &|_| false, control)
                })
                .map(|result| result.unwrap_or(false));
            visiting.leave(&key);
            result
        }
        SourcePlan::Join {
            left, right, on, ..
        } => {
            if let Some(on) = on {
                if scalar_calls_are_volatile_with_control(catalog, on, control)? {
                    return Ok(true);
                }
            }
            Ok(source_inner(catalog, left, visiting, scope, control)?
                || source_inner(catalog, right, visiting, scope, control)?)
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter().flatten() {
                if scalar_calls_are_volatile_with_control(catalog, expression, control)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => {
            if function_volatility_with_binding_and_control(
                catalog,
                name,
                binding.as_ref(),
                args.len(),
                control,
            )? == FunctionVolatility::Volatile
            {
                return Ok(true);
            }
            for argument in args {
                if scalar_calls_are_volatile_with_control(catalog, argument, control)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if function_volatility_with_binding_and_control(
                    catalog,
                    &function.name,
                    function.binding.as_ref(),
                    function.args.len(),
                    control,
                )? == FunctionVolatility::Volatile
                {
                    return Ok(true);
                }
                for argument in &function.args {
                    if scalar_calls_are_volatile_with_control(catalog, argument, control)? {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        SourcePlan::Subquery { body, .. } => query_inner(catalog, body, visiting, scope, control),
    }
}

pub fn unified_plan_contains_volatile_function_with_control(
    catalog: &dyn VolatilityCatalog,
    plan: &UnifiedPlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let mut result = Ok(false);
    plan.visit_scalar_expressions(&mut |expression| {
        if matches!(result, Ok(false)) {
            result = scalar_calls_are_volatile_with_control(catalog, expression, control);
        }
    });
    result
}
