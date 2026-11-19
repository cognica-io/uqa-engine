//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Attribute validation retains only the fixed `PostgreSQL` slot domain and yields between declarations.

use super::{too_many_columns, ColumnDef, DroppedAttribute, MAX_ATTRIBUTES};
use crate::{catalog::work::Work, SQLError};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};

pub fn validate_with_control(
    columns: &[ColumnDef],
    dropped: &[DroppedAttribute],
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate_from(columns, dropped, control, &mut ()))
}

pub async fn validate_from_input(
    columns: &[ColumnDef],
    dropped: &[DroppedAttribute],
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    validate_from(columns, dropped, control, input).await
}

pub(super) async fn validate_from(
    columns: &[ColumnDef],
    dropped: &[DroppedAttribute],
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    // Complete missing-number validation precedes every layout error.
    for column in columns {
        work.checkpoint().await?;
        control.check()?;
        if column.attribute_number.is_none() {
            return Err(SQLError::Internal(
                "relation attributes require initial catalog migration".into(),
            ));
        }
    }
    numbers_from(
        columns
            .iter()
            .map(|column| column.attribute_number.expect("complete attribute numbers")),
        dropped,
        control,
        work,
    )
    .await
}

pub(super) async fn numbers_from(
    numbers: impl Iterator<Item = i16>,
    dropped: &[DroppedAttribute],
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    control.check()?;
    let mut layout = Layout::default();
    let mut previous = None;
    for number in numbers {
        work.checkpoint().await?;
        control.check()?;
        layout.invalid |= previous.is_some_and(|previous| previous >= number);
        previous = Some(number);
        layout.include(number);
    }
    for attribute in dropped {
        work.checkpoint().await?;
        control.check()?;
        layout.include(attribute.number);
    }
    control.check()?;
    if layout.excess {
        return Err(too_many_columns());
    }
    if layout.invalid || layout.count != layout.maximum {
        return Err(SQLError::Internal(
            "invalid relation attribute slot layout".into(),
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Layout {
    slots: [u64; 25],
    count: usize,
    maximum: usize,
    invalid: bool,
    excess: bool,
}

impl Layout {
    fn include(&mut self, number: i16) {
        if number > MAX_ATTRIBUTES {
            self.excess = true;
        } else if number <= 0 {
            self.invalid = true;
        } else {
            let number = usize::try_from(number).expect("positive attribute number");
            let index = number - 1;
            let mask = 1_u64 << (index % 64);
            let word = &mut self.slots[index / 64];
            if *word & mask != 0 {
                self.invalid = true;
            } else {
                *word |= mask;
                self.count += 1;
                self.maximum = self.maximum.max(number);
            }
        }
    }
}

#[cfg(test)]
mod tests;
