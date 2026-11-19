//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics publication caches exposed native allocations before sharing its immutable generation.

use super::{
    AdmittedColumns, BTreeMap, ColumnName, ColumnStats, Produced, ProductionControl, SQLError,
};
use uqa_core::memory::{MemoryError, OwnedMap};

fn add(total: &mut usize, bytes: usize) -> Result<(), SQLError> {
    *total = total.checked_add(bytes).ok_or(MemoryError::SizeOverflow)?;
    Ok(())
}

fn buffer<T>(count: usize) -> Result<usize, SQLError> {
    Ok(count
        .checked_mul(size_of::<T>())
        .ok_or(MemoryError::SizeOverflow)?)
}

pub(super) fn column(
    name: &String,
    stats: &ColumnStats,
    control: &ProductionControl<'_>,
    bytes: &mut usize,
) -> Result<(), SQLError> {
    control.check()?;
    add(bytes, name.capacity())?;
    add(
        bytes,
        buffer::<uqa_core::Value>(stats.histogram.capacity())?,
    )?;
    add(
        bytes,
        buffer::<uqa_core::Value>(stats.mcv_values.capacity())?,
    )?;
    add(bytes, buffer::<f64>(stats.mcv_frequencies.capacity())?)?;
    for value in stats
        .min_value
        .iter()
        .chain(stats.max_value.iter())
        .chain(&stats.histogram)
        .chain(&stats.mcv_values)
    {
        add(bytes, value.retained_payload_bytes_with_control(control)?)?;
    }
    Ok(())
}

pub(super) fn replacement(
    bytes: usize,
    name: &String,
    previous: Option<&ColumnStats>,
    next: &ColumnStats,
) -> Result<usize, SQLError> {
    let control = ProductionControl::uncontrolled();
    let mut old = 0;
    if let Some(previous) = previous {
        column(name, previous, &control, &mut old)?;
    }
    let mut result = bytes.checked_sub(old).ok_or(MemoryError::SizeOverflow)?;
    if previous.is_none() {
        add(&mut result, size_of::<(String, ColumnStats)>())?;
    }
    column(name, next, &control, &mut result)?;
    Ok(result)
}

pub(super) fn ordinary(
    columns: &BTreeMap<String, ColumnStats>,
    control: &ProductionControl<'_>,
) -> Result<usize, SQLError> {
    // Standard B-tree node slack is not exposed; retain the header and live key/value layouts, matching Core's native map payload contract.
    let mut bytes = size_of::<BTreeMap<String, ColumnStats>>();
    add(&mut bytes, buffer::<(String, ColumnStats)>(columns.len())?)?;
    for (name, stats) in columns {
        column(name, stats, control, &mut bytes)?;
    }
    Ok(bytes)
}

pub(super) fn admitted(
    columns: &AdmittedColumns,
    control: &ProductionControl<'_>,
) -> Result<usize, SQLError> {
    let mut bytes = size_of::<Produced<AdmittedColumns>>();
    add(
        &mut bytes,
        columns
            .len()
            .checked_mul(OwnedMap::<ColumnName, Produced<ColumnStats>>::entry_bytes())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    for (name, stats) in columns {
        column(&name.0, stats, control, &mut bytes)?;
    }
    Ok(bytes)
}
