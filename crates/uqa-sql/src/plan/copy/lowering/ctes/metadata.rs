//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTE traversal metadata keeps independent mark/default query scopes and scalar retirement until publication.

use super::super::{
    input::{text, ScalarInput, TextInput},
    Copying, Result,
};
use crate::{
    ast::{CteCycleClause, CteSearchClause, Expr},
    plan::{retirement::Pending, CteCyclePlan, CteSearchPlan},
};

pub(super) async fn construct_search<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s CteSearchClause,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<CteSearchPlan> {
    let columns = text::texts(&source.columns, copying, input).await?;
    let sequence_column = copying.take(input.text(&source.sequence_column, copying.control).await?);
    Ok(CteSearchPlan {
        columns,
        breadth_first: source.breadth_first,
        sequence_column,
    })
}

pub(in crate::plan::copy::lowering) async fn construct_cycle<
    's,
    M: ScalarInput<&'s Expr> + TextInput<&'s str>,
    D: ScalarInput<&'s Expr>,
>(
    source: &'s CteCycleClause,
    copying: &mut Copying<'_>,
    mark: &mut M,
    default: &mut D,
) -> Result<Pending<CteCyclePlan>> {
    let columns = text::texts(&source.columns, copying, mark).await?;
    let mark_column = copying.take(mark.text(&source.mark_column, copying.control).await?);
    let mut field = copying.begin_input_field()?;
    let mark_value = mark.scalar(&source.mark_value, copying.control).await?;
    let mark_value = Pending::new(field.take_scalar(mark_value)?);
    copying.finish_input_field("mark_value", field);
    let mut field = copying.begin_input_field()?;
    let mark_default = default
        .scalar(&source.mark_default, copying.control)
        .await?;
    let mark_default = Pending::new(field.take_scalar(mark_default)?);
    copying.finish_input_field("mark_default", field);
    let path_column = copying.take(mark.text(&source.path_column, copying.control).await?);
    Ok(Pending::new(CteCyclePlan {
        columns,
        mark_column,
        mark_value: mark_value.take(),
        mark_default: mark_default.take(),
        path_column,
    }))
}
