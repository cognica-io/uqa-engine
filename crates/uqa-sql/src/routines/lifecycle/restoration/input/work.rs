//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RoutineRestoreDiagnostic, RoutineRestoreInputError};
use std::cmp::Ordering;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MapKeyInput, Produced, ProductionControl, ProductionString},
};

pub(super) async fn checkpoint(
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), RoutineRestoreInputError> {
    control.check()?;
    input.checkpoint().await?;
    control.check()?;
    Ok(())
}

pub(super) async fn append(
    output: &mut ProductionString<'_>,
    text: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), RoutineRestoreInputError> {
    let mut begin = 0;
    while begin < text.len() {
        checkpoint(control, input).await?;
        let mut end = (begin + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&text[begin..end])?;
        begin = end;
    }
    Ok(())
}

pub(super) async fn text(
    value: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<String>, RoutineRestoreInputError> {
    let mut output = ProductionString::new(*control);
    append(&mut output, value, control, input).await?;
    Ok(output.finish()?)
}

pub(super) async fn message(
    parts: &[&str],
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<RoutineRestoreInputError, RoutineRestoreInputError> {
    let mut output = ProductionString::new(*control);
    for part in parts {
        checkpoint(control, input).await?;
        append(&mut output, part, control, input).await?;
    }
    Ok(RoutineRestoreDiagnostic::message(output.finish()?).into())
}

pub(super) async fn compare(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Ordering, RoutineRestoreInputError> {
    checkpoint(control, input).await?;
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        checkpoint(control, input).await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

pub(super) async fn signature_eq(
    left: &[String],
    right: &[String],
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, RoutineRestoreInputError> {
    checkpoint(control, input).await?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        if compare(left, right, control, input).await?.is_ne() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) struct Comparison<'a, 'b, 'c> {
    pub control: ProductionControl<'a>,
    pub input: &'b mut AsyncImageInput<'c>,
}

impl MapKeyInput<str, String> for Comparison<'_, '_, '_> {
    type Error = RoutineRestoreInputError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        checkpoint(&self.control, self.input).await
    }
    async fn compare(&mut self, query: &str, stored: &String) -> Result<Ordering, Self::Error> {
        compare(query, stored, &self.control, self.input).await
    }
}

impl MapKeyInput<String> for Comparison<'_, '_, '_> {
    type Error = RoutineRestoreInputError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        checkpoint(&self.control, self.input).await
    }
    async fn compare(&mut self, query: &String, stored: &String) -> Result<Ordering, Self::Error> {
        compare(query, stored, &self.control, self.input).await
    }
}
