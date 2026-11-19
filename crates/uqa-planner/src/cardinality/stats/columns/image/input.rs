//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, value_error, ColumnStatistics, ColumnStats, SQLError, MAGIC};
use crate::cardinality::stats::columns::Builder;
use std::{cmp::Ordering, io::Read};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput},
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, Produced, ProductionControl},
    value_image::ValueImageInput,
    CancellationToken, Value,
};

fn take<T>(value: Budgeted<T>, memory: &mut MemoryReservation) -> T {
    let (value, reservation) = value.into_parts();
    memory.absorb(reservation);
    value
}

async fn flag(input: &mut AsyncImageInput<'_>) -> Result<bool, SQLError> {
    match input.array::<1>().await?[0] {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid("optional value tag")),
    }
}

async fn value(input: &mut AsyncImageInput<'_>) -> Result<Budgeted<Value>, SQLError> {
    ValueImageInput::read_from(input).await.map_err(value_error)
}

async fn values(
    input: &mut AsyncImageInput<'_>,
    count: usize,
    memory: &mut MemoryReservation,
) -> Result<Vec<Value>, SQLError> {
    let mut values = BudgetedVec::new(input.memory);
    values.reserve(count)?;
    for _ in 0..count {
        values.push(take(value(input).await?, memory))?;
    }
    let (values, reservation) = values.into_parts();
    memory.absorb(reservation);
    Ok(values)
}

async fn distribution(input: &mut AsyncImageInput<'_>) -> Result<Budgeted<ColumnStats>, SQLError> {
    let distinct_count = u64::from_le_bytes(input.array().await?);
    let null_count = u64::from_le_bytes(input.array().await?);
    let row_count = u64::from_le_bytes(input.array().await?);
    let min = flag(input).await?;
    let max = flag(input).await?;
    let histogram_count = input.count().await?;
    let common_count = input.count().await?;
    let frequency_count = input.count().await?;
    let mut memory = input.memory.empty_reservation();
    let min_value = if min {
        Some(take(value(input).await?, &mut memory))
    } else {
        None
    };
    let max_value = if max {
        Some(take(value(input).await?, &mut memory))
    } else {
        None
    };
    let histogram = values(input, histogram_count, &mut memory).await?;
    let mcv_values = values(input, common_count, &mut memory).await?;
    let mut frequencies = BudgetedVec::new(input.memory);
    frequencies.reserve(frequency_count)?;
    for _ in 0..frequency_count {
        frequencies.push(f64::from_bits(u64::from_le_bytes(input.array().await?)))?;
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

async fn increasing(
    left: &str,
    right: &str,
    input: &mut AsyncImageInput<'_>,
) -> Result<bool, SQLError> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        input.checkpoint().await?;
        match left.cmp(right) {
            Ordering::Less => return Ok(true),
            Ordering::Greater => return Ok(false),
            Ordering::Equal => {}
        }
    }
    input.cancellation.check()?;
    Ok(left.len() < right.len())
}

pub(super) fn read(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<ColumnStatistics, SQLError> {
    decode_image(reader, memory, cancellation, async |input| {
        read_from(input).await
    })
}

pub(super) async fn read_from(
    mut input: AsyncImageInput<'_>,
) -> Result<ColumnStatistics, SQLError> {
    let memory = input.memory;
    let cancellation = input.cancellation;
    if input.array::<8>().await? != MAGIC {
        return Err(invalid("version header"));
    }
    let count = input.count().await?;
    let control = ProductionControl::new(memory, cancellation, cancellation);
    let mut columns = Builder::new(control);
    let mut pending: Option<(Produced<String>, Produced<ColumnStats>)> = None;
    for _ in 0..count {
        let name = input.text().await?;
        if name.is_empty() {
            return Err(invalid("column name order"));
        }
        if let Some((previous, _)) = &pending {
            if !increasing(previous, &name, &mut input).await? {
                return Err(invalid("column name order"));
            }
        }
        if let Some((name, stats)) = pending.take() {
            columns.insert_owned(name, stats)?;
        }
        let (name, reservation) = name.into_parts();
        let name = control.finish(name, Some(reservation))?;
        let (stats, reservation) = distribution(&mut input).await?.into_parts();
        pending = Some((name, control.finish(stats, Some(reservation))?));
    }
    input.end().await?;
    if let Some((name, stats)) = pending {
        columns.insert_owned(name, stats)?;
    }
    columns.finish()
}
