//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-consumer CTE predicates retain admitted names and expressions.

use super::{
    context::{FilterPushdownContext, FilterPushdownScope},
    source_columns::SourceColumns,
};
use uqa_core::memory::{ProductionControl, ProductionMap};
use uqa_sql::{
    binding::production::BindingMap,
    plan::{source_projection::CteOutputFilters, QueryPlan, RelationalPlan, SourcePlan},
    semantics::{
        expr_contains_subquery, qualify_unqualified_columns_with_control,
        visit_filter_parts_with_control, volatility::expr_contains_volatile_function_with_control,
    },
    SQLError,
};

pub fn cte_output_filters_with_control(
    context: FilterPushdownContext<'_>,
    plan: &QueryPlan,
    scope: FilterPushdownScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<CteOutputFilters, SQLError> {
    control.check()?;
    let mut output = CteOutputFilters::new();
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return Ok(output);
    };
    let (Some(from), Some(filter)) = (&block.from, &block.r#where) else {
        return Ok(output);
    };
    if expr_contains_subquery(filter)
        || expr_contains_volatile_function_with_control(context.volatility, filter, control)?
    {
        return Ok(output);
    }
    let mut names = ProductionMap::new(*control);
    for cte in &plan.ctes {
        names.insert_copy(
            uqa_core::RelationName::decoded(&cte.name),
            cte.name.as_str(),
        )?;
    }
    let mut references = ProductionMap::new(*control);
    collect_references(from, &names, control, &mut references)?;
    let mut qualifier_to_cte = ProductionMap::new(*control);
    for (cte, qualifier) in &references {
        if let Some(qualifier) = qualifier {
            qualifier_to_cte.insert_copy(*qualifier, *cte)?;
        }
    }
    if qualifier_to_cte.is_empty() {
        return Ok(output);
    }
    let columns = SourceColumns::new_with_control(scope, from, control)?;
    let mut grouped =
        BindingMap::<(&str, crate::scratch::Buffer<uqa_sql::plan::OwnedScalarExpr>)>::new();
    visit_filter_parts_with_control(filter, control, &mut |part| {
        let Some(qualifier) = super::placement::qualifier_for_part(
            context,
            part,
            &columns,
            &block.subqueries,
            control,
        )?
        else {
            return Ok(());
        };
        let Some(cte) = qualifier_to_cte.get(qualifier) else {
            return Ok(());
        };
        let predicate = qualify_unqualified_columns_with_control(part, qualifier, control)?;
        if let Some((_, predicates)) = grouped.get_mut(cte) {
            predicates.push(predicate)?;
        } else {
            let mut predicates = crate::scratch::Buffer::new(*control);
            predicates.push(predicate)?;
            grouped.insert_with_control(cte, (qualifier, predicates), control)?;
        }
        Ok(())
    })?;
    // The stable reference map supplies the existing CTE name order; removing a group retires its temporary key and node.
    for (cte, _) in &references {
        if let Some((qualifier, predicates)) = grouped.remove(cte) {
            if let Some(predicate) = super::placement::combine_owned(predicates, true, control)? {
                output.insert_with_control(
                    cte,
                    (control.copy_text(qualifier)?, predicate),
                    control,
                )?;
            }
        }
    }
    Ok(output)
}
fn collect_references<'a>(
    source: &'a SourcePlan,
    names: &ProductionMap<'_, uqa_core::RelationName<'a>, &'a str>,
    control: &ProductionControl<'_>,
    references: &mut ProductionMap<'_, &'a str, Option<&'a str>>,
) -> Result<(), SQLError> {
    control.check()?;
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => {
            if let Some(reference) = uqa_core::RelationIdentity::borrowed_reference(name)
                .filter(|reference| reference.schema.is_none())
            {
                if let Some(cte) = names.get(&reference.name) {
                    let qualifier = if references.get(*cte).is_some() {
                        None
                    } else {
                        Some(alias.as_deref().unwrap_or(qualifier))
                    };
                    references.insert_copy(*cte, qualifier)?;
                }
            }
        }
        SourcePlan::Join { left, right, .. } => {
            collect_references(left, names, control, references)?;
            collect_references(right, names, control, references)?;
        }
        _ => {}
    }
    Ok(())
}
