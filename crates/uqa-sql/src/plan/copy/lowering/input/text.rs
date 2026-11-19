//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Names retain per-item constructor records across bounded text input.

use super::{Copying, Result, ScalarInput, TextInput};
use crate::ast::Expr;
use uqa_core::memory::BudgetedVec;

pub(in crate::plan::copy) async fn returning_aliases<'s, I: TextInput<&'s str>>(
    source: &'s crate::ast::ReturningAliases,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<crate::ast::ReturningAliases> {
    Ok(crate::ast::ReturningAliases {
        old: copying.take(input.text(&source.old, copying.control).await?),
        new: copying.take(input.text(&source.new, copying.control).await?),
        old_explicit: source.old_explicit,
        new_explicit: source.new_explicit,
    })
}

pub(in crate::plan::copy::lowering) async fn optional_text<'s, I: TextInput<&'s str>>(
    source: Option<&'s str>,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Option<String>> {
    match source {
        Some(source) => Ok(Some(
            copying.take(input.text(source, copying.control).await?),
        )),
        None => Ok(None),
    }
}

pub(in crate::plan::copy::lowering) async fn texts<
    's,
    I: ScalarInput<&'s Expr> + TextInput<&'s str>,
>(
    source: &'s [String],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Vec<String>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = BudgetedVec::new(copying.control.budget().expect("mutation name allowance"));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("name records")
        .reserve(source.len())?;
    for value in source {
        input.checkpoint().await?;
        copying.check()?;
        let mut field = copying.begin_input_field()?;
        values
            .push(field.take(input.text(value, copying.control).await?))
            .expect("admitted name slot");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.into_parts();
    copying.absorb(Some(memory));
    Ok(values)
}
