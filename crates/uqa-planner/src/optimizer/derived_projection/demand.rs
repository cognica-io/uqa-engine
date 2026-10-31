//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Direct-source output demand borrows consumer references while retaining only positional results.

use super::{QueryBlockPlan, SQLError, ScalarExpr, SourcePlan};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

pub(super) fn consumer_outputs<'a>(
    block: &QueryBlockPlan,
    control: ProductionControl<'a>,
) -> Result<Option<ProductionVec<'a, bool>>, SQLError> {
    let Some(SourcePlan::Subquery {
        body,
        alias,
        column_aliases,
        ..
    }) = &block.from
    else {
        return Ok(None);
    };
    if !block.subqueries.is_empty() {
        return Ok(None);
    }
    let mut required = ProductionMap::new(control);
    let mut all = false;
    block.try_visit_expressions(&mut |expression| {
        control.check()?;
        super::super::output_demand::try_visit(expression, &control, &mut |part| {
            match part {
                ScalarExpr::Column(name) => {
                    if alias.as_deref() == Some(name.as_str())
                        && !super::has_named_output(body, column_aliases, name, &control)?
                    {
                        all = true;
                    } else {
                        required.insert_copy(name.as_str(), ())?;
                    }
                }
                ScalarExpr::QualifiedColumn { qualifier, column }
                    if alias.as_deref() == Some(qualifier.as_str()) =>
                {
                    required.insert_copy(column.as_str(), ())?;
                }
                ScalarExpr::QualifiedStar(qualifier)
                    if alias.as_deref() == Some(qualifier.as_str()) =>
                {
                    all = true;
                }
                ScalarExpr::Star | ScalarExpr::Position(_) => all = true,
                _ => {}
            }
            Ok::<_, SQLError>(())
        })?;
        Ok::<_, SQLError>(true)
    })?;
    if all {
        return Ok(None);
    }
    super::output_demand(
        body,
        column_aliases,
        &|name| required.get(name).is_some(),
        None,
        control,
    )
}
