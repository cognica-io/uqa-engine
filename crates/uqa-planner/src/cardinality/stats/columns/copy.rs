//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{ColumnStats, Produced, ProductionControl, SQLError};
use uqa_core::{memory::ProductionVec, Value};

pub(super) fn column(
    stats: &ColumnStats,
    control: ProductionControl<'_>,
) -> Result<Produced<ColumnStats>, SQLError> {
    let min = stats
        .min_value
        .as_ref()
        .map(|value| control.copy_value(value))
        .transpose()?;
    let max = stats
        .max_value
        .as_ref()
        .map(|value| control.copy_value(value))
        .transpose()?;
    let histogram = values(&stats.histogram, control)?;
    let common = values(&stats.mcv_values, control)?;
    let mut frequencies = ProductionVec::new(control);
    frequencies.extend_from_slice(&stats.mcv_frequencies)?;
    let frequencies = frequencies.finish()?;
    // All fallible production finishes before any payload is separated from its lease.
    let (histogram, mut memory) = histogram.into_parts();
    let (mcv_values, owned) = common.into_parts();
    memory = control.combine(memory, owned);
    let (mcv_frequencies, owned) = frequencies.into_parts();
    memory = control.combine(memory, owned);
    let min_value = min.map(|value| {
        let (value, owned) = value.into_parts();
        memory = control.combine(memory.take(), owned);
        value
    });
    let max_value = max.map(|value| {
        let (value, owned) = value.into_parts();
        memory = control.combine(memory.take(), owned);
        value
    });
    Ok(control.finish(
        ColumnStats {
            distinct_count: stats.distinct_count,
            null_count: stats.null_count,
            row_count: stats.row_count,
            min_value,
            max_value,
            histogram,
            mcv_values,
            mcv_frequencies,
        },
        memory,
    )?)
}

fn values(
    values: &[Value],
    control: ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    let mut result = ProductionVec::new(control);
    for value in values {
        result.push_produced(control.copy_value(value)?)?;
    }
    Ok(result.finish()?)
}
