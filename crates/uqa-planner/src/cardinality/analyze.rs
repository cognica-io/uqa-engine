//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact distribution statistics from stable Core-ordered observations, without population-sized maps.

use super::{ColumnStatistics, ColumnStats};
use std::cmp::Ordering;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};
use uqa_sql::SQLError;

mod distribution;
use crate::scratch::Buffer;
use distribution::Distribution;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
pub struct ColumnObservationCounts {
    pub nulls: u64,
    pub omitted: u64,
    pub comparable: u64,
}

/// Storage and physical sorting belong to the caller. This fold retains only one equality group and the requested histogram/MCV output per column.
pub fn collect_statistics(
    row_count: u64,
    counts: &[ColumnObservationCounts],
    values: impl IntoIterator<Item = Result<(usize, Value), SQLError>>,
    histogram_values: usize,
    mcv_values: usize,
) -> Result<Vec<ColumnStats>, SQLError> {
    Ok(collect_statistics_with_control(
        row_count,
        counts,
        values,
        histogram_values,
        mcv_values,
        ProductionControl::uncontrolled(),
    )?
    .into_uncontrolled()
    .expect("ordinary statistics producer"))
}

/// Admit distribution scratch and retained copies before allocation. Incoming observations retain their caller's storage ownership; this producer borrows each observation and owns its own copies. Comparisons use Core's controlled value ordering, including its temporary numeric and JSON storage.
pub fn collect_statistics_with_control(
    row_count: u64,
    counts: &[ColumnObservationCounts],
    values: impl IntoIterator<Item = Result<(usize, Value), SQLError>>,
    histogram_values: usize,
    mcv_values: usize,
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<ColumnStats>>, SQLError> {
    let mut output = ProductionVec::new(control);
    fold_statistics(
        row_count,
        counts.iter().copied().map(|count| ((), count)),
        values,
        histogram_values,
        mcv_values,
        control,
        |(), stats| Ok(output.push_produced(stats)?),
    )?;
    Ok(output.finish()?)
}

/// Publish a completed immutable catalog generation while retaining each column's admitted output. Duplicate names replace the earlier entry and release its payload immediately, as in the ordinary map collector.
pub fn collect_named_statistics<'a>(
    row_count: u64,
    columns: impl IntoIterator<Item = (&'a str, ColumnObservationCounts)>,
    values: impl IntoIterator<Item = Result<(usize, Value), SQLError>>,
    histogram_values: usize,
    mcv_values: usize,
    control: ProductionControl<'_>,
) -> Result<ColumnStatistics, SQLError> {
    let mut output = super::stats::columns::Builder::new(control);
    fold_statistics(
        row_count,
        columns,
        values,
        histogram_values,
        mcv_values,
        control,
        |name, stats| output.insert(name, stats),
    )?;
    output.finish()
}

fn fold_statistics<N>(
    row_count: u64,
    counts: impl IntoIterator<Item = (N, ColumnObservationCounts)>,
    values: impl IntoIterator<Item = Result<(usize, Value), SQLError>>,
    histogram_values: usize,
    mcv_values: usize,
    control: ProductionControl<'_>,
    mut retain: impl FnMut(N, Produced<ColumnStats>) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    if histogram_values < 2 {
        return Err(invalid("ANALYZE histogram requires two boundary slots"));
    }
    control.check()?;
    let mut columns = Buffer::new(control);
    for (name, counts) in counts {
        control.check()?;
        columns.push((
            name,
            Distribution::new(row_count, counts, histogram_values, mcv_values, control),
        ))?;
    }
    for item in values {
        let (column, value) = item?;
        control.check()?;
        columns
            .get_mut(column)
            .ok_or_else(|| invalid("ANALYZE value has an unknown column"))?
            .1
            .observe(&value)?;
    }
    for (name, column) in columns {
        control.check()?;
        retain(name, column.finish()?)?;
    }
    Ok(())
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(message.into())
}

fn overflow() -> SQLError {
    invalid("ANALYZE observation count overflow")
}
