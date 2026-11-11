//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored distributions remain encoded until their Planner owner interprets them.

use crate::ColumnStatsRow;
use uqa_core::memory::Budgeted;

/// Adopt the original name/distribution/bound buffers and their reservations without parsing or normalizing statistical values.
pub fn row(
    fields: [Budgeted<String>; 4],
    bounds: [Option<Budgeted<String>>; 2],
    counts: [i64; 3],
) -> Budgeted<ColumnStatsRow> {
    let [column, histogram, values, frequencies] = fields;
    let (column_name, mut memory) = column.into_parts();
    let [histogram_json, mcv_values_json, mcv_frequencies_json] = [histogram, values, frequencies]
        .map(|field| {
            let (field, reservation) = field.into_parts();
            memory.absorb(reservation);
            field
        });
    let [min_value, max_value] = bounds.map(|field| {
        field.map(|field| {
            let (field, reservation) = field.into_parts();
            memory.absorb(reservation);
            field
        })
    });
    let [distinct_count, null_count, row_count] = counts;
    Budgeted::new(
        ColumnStatsRow {
            column_name,
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram_json,
            mcv_values_json,
            mcv_frequencies_json,
        },
        memory,
    )
}
