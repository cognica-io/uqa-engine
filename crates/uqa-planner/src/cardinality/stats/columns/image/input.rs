//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, value_error, ColumnStatistics, ColumnStats, SQLError, MAGIC};
use crate::cardinality::stats::columns::Builder;
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    image_io::ImageInput,
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, Produced, ProductionControl},
    value_image::read_value_image,
    CancellationToken, Value,
};

fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, reservation) = value.into_parts();
    memory.absorb(reservation);
    value
}

fn flag(input: &mut ImageInput<'_>) -> Result<bool, SQLError> {
    match input.array::<1>()?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("optional value tag")),
    }
}

fn value(input: &mut ImageInput<'_>) -> Result<Budgeted<Value>, SQLError> {
    read_value_image(input.reader, input.memory, input.cancellation).map_err(value_error)
}

fn values(
    input: &mut ImageInput<'_>,
    count: usize,
    memory: &mut MemoryReservation,
) -> Result<Vec<Value>, SQLError> {
    let mut values = BudgetedVec::new(input.memory);
    values.reserve(count)?;
    for _ in 0..count {
        values.push(take(value(input)?, memory))?;
    }
    let (values, reservation) = values.into_parts();
    memory.absorb(reservation);
    Ok(values)
}

fn distribution(input: &mut ImageInput<'_>) -> Result<Budgeted<ColumnStats>, SQLError> {
    let distinct_count = u64::from_le_bytes(input.array()?);
    let null_count = u64::from_le_bytes(input.array()?);
    let row_count = u64::from_le_bytes(input.array()?);
    let min = flag(input)?;
    let max = flag(input)?;
    let histogram_count = input.count()?;
    let common_count = input.count()?;
    let frequency_count = input.count()?;
    let mut memory = input.memory.empty_reservation();
    let min_value = min
        .then(|| value(input).map(|v| take(v, &mut memory)))
        .transpose()?;
    let max_value = max
        .then(|| value(input).map(|v| take(v, &mut memory)))
        .transpose()?;
    let histogram = values(input, histogram_count, &mut memory)?;
    let mcv_values = values(input, common_count, &mut memory)?;
    let mut frequencies = BudgetedVec::new(input.memory);
    frequencies.reserve(frequency_count)?;
    for _ in 0..frequency_count {
        frequencies.push(f64::from_bits(u64::from_le_bytes(input.array()?)))?;
    }
    let (mcv_frequencies, reservation) = frequencies.into_parts();
    memory.absorb(reservation);
    Ok(Budgeted::new(
        ColumnStats {
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram,
            mcv_values,
            mcv_frequencies,
        },
        memory,
    ))
}

fn increasing(left: &str, right: &str, token: &CancellationToken) -> Result<bool, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        token.check()?;
        match left.cmp(right) {
            Ordering::Less => return Ok(true),
            Ordering::Greater => return Ok(false),
            Ordering::Equal => {}
        }
    }
    token.check()?;
    Ok(left.len() < right.len())
}

pub(super) fn read(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ColumnStatistics, SQLError> {
    let mut input = ImageInput {
        reader,
        memory,
        cancellation,
    };
    if input.array::<8>()? != MAGIC {
        return Err(invalid("version header"));
    }
    let count = input.count()?;
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let mut columns = Builder::new(control);
    let mut pending: Option<(Produced<String>, Produced<ColumnStats>)> = None;
    for _ in 0..count {
        let name = input.text()?;
        if name.is_empty()
            || pending
                .as_ref()
                .map(|(previous, _)| increasing(previous, &name, cancellation))
                .transpose()?
                == Some(false)
        {
            return Err(invalid("column name order"));
        }
        if let Some((name, stats)) = pending.take() {
            columns.insert_owned(name, stats)?;
        }
        let (name, reservation) = name.into_parts();
        let name = control.finish(name, Some(reservation))?;
        let (stats, reservation) = distribution(&mut input)?.into_parts();
        pending = Some((name, control.finish(stats, Some(reservation))?));
    }
    input.end()?;
    if let Some((name, stats)) = pending {
        columns.insert_owned(name, stats)?;
    }
    columns.finish()
}
