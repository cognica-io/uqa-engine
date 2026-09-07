//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded reservoir and projected column values across a relation hierarchy.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_core::DocId;
use uqa_storage::{DocumentStore, StorageBackendError, StorageBackendResult};

use super::super::{build_analyze_stats, collect_analyze_values, HierarchyAnalyzeInputs};
use crate::{ColumnStatsMap, Engine};

const SAMPLE_ROWS: usize = 4_096;
const ID_BATCH: usize = 512;

struct Reservoir {
    selected: Vec<(usize, DocId)>,
    seen: u64,
    random: u64,
}

impl Reservoir {
    fn new() -> Self {
        Self {
            selected: Vec::with_capacity(SAMPLE_ROWS),
            seen: 0,
            random: 0x9e37_79b9_7f4a_7c15,
        }
    }

    fn observe(&mut self, member: usize, id: DocId) {
        self.seen += 1;
        if self.selected.len() < SAMPLE_ROWS {
            self.selected.push((member, id));
            return;
        }
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        let position = self.random % self.seen;
        if position < SAMPLE_ROWS as u64 {
            self.selected[position as usize] = (member, id);
        }
    }
}

fn sample_ids(
    engine: &Engine,
    name: &str,
) -> StorageBackendResult<(Reservoir, Vec<Arc<dyn DocumentStore>>)> {
    let members = engine
        .hierarchy_scan_tables(name, true)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    let mut reservoir = Reservoir::new();
    let mut snapshots = Vec::new();
    for member in members {
        let Some(table) = engine.try_table(&member)? else {
            continue;
        };
        let snapshot = table.document_store.read().snapshot()?;
        let mut after = None;
        loop {
            engine
                .runtime
                .cancellation
                .check()
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            let ids = snapshot.next_doc_ids(after, ID_BATCH)?;
            if ids.is_empty() {
                break;
            }
            after = ids.last().copied();
            for id in ids {
                reservoir.observe(snapshots.len(), id);
            }
        }
        snapshots.push(snapshot);
    }
    Ok((reservoir, snapshots))
}

pub(super) fn collect(
    engine: &Engine,
    name: &str,
    columns: &[String],
) -> StorageBackendResult<(ColumnStatsMap, u64)> {
    let (reservoir, snapshots) = sample_ids(engine, name)?;
    let mut values = columns
        .iter()
        .map(|column| (column.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let mut null_counts = columns
        .iter()
        .map(|column| (column.clone(), 0_u64))
        .collect::<BTreeMap<_, _>>();
    for (index, snapshot) in snapshots.iter().enumerate() {
        let mut selected = reservoir
            .selected
            .iter()
            .filter_map(|(member, id)| (*member == index).then_some(*id))
            .collect::<Vec<_>>();
        selected.sort_unstable();
        let (mut member_values, member_nulls) =
            collect_analyze_values(snapshot.as_ref(), &selected, columns)?;
        for column in columns {
            values
                .get_mut(column)
                .expect("initialized sample column")
                .extend(member_values.remove(column).unwrap_or_default());
            *null_counts
                .get_mut(column)
                .expect("initialized sample null counter") += member_nulls[column];
        }
    }
    let sample_count = reservoir.selected.len() as u64;
    let mut statistics = build_analyze_stats(
        columns,
        HierarchyAnalyzeInputs {
            row_count: sample_count,
            values,
            null_counts,
        },
    )?;
    extrapolate(&mut statistics, sample_count, reservoir.seen);
    Ok((statistics, reservoir.seen))
}

fn extrapolate(statistics: &mut ColumnStatsMap, sample_count: u64, row_count: u64) {
    if sample_count == 0 || sample_count == row_count {
        return;
    }
    for value in statistics.values_mut() {
        value.null_count =
            (value.null_count as f64 * row_count as f64 / sample_count as f64).round() as u64;
        // Scale high-distinctness estimates; retain observed support for
        // low-cardinality categories. Cost estimates do not affect results.
        if value.distinct_count * 10 > sample_count {
            value.distinct_count = ((value.distinct_count as f64 * row_count as f64
                / sample_count as f64)
                .round() as u64)
                .min(row_count - value.null_count);
        }
        value.row_count = row_count;
    }
}
