//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored distributions retain the same value selection and validation order as ordinary catalog loading.

use super::{columns::Builder, ColumnStatistics, ColumnStats};
use uqa_core::{
    json::JsonReadError,
    memory::{
        Budgeted, MemoryBudget, MemoryReservation, Produced, ProductionControl, ProductionVec,
    },
    CancellationToken, Value, ValueRetentionError,
};
use uqa_sql::SQLError;

mod input;
pub mod limits;
use input::Decoder;
#[cfg(test)]
mod tests;

/// Borrowed provider-neutral fields from one durable column-statistics row.
#[derive(Clone, Copy)]
pub struct StoredColumnStats<'a> {
    pub column_name: &'a str,
    pub distinct_count: i64,
    pub null_count: i64,
    pub min_value: Option<&'a str>,
    pub max_value: Option<&'a str>,
    pub row_count: i64,
    pub histogram_json: &'a str,
    pub mcv_values_json: &'a str,
    pub mcv_frequencies_json: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum StoredColumnStatsError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Input(#[from] JsonReadError),
    #[error(transparent)]
    Resource(#[from] ValueRetentionError),
    #[error(transparent)]
    Publication(#[from] SQLError),
    #[error("{0}")]
    Invalid(String),
}

type Result<T> = std::result::Result<T, StoredColumnStatsError>;

/// A partially decoded distribution drops all values before their combined allowance.
struct Distribution {
    value: ColumnStats,
    memory: Option<MemoryReservation>,
}

fn take<T>(
    value: Produced<T>,
    memory: &mut Option<MemoryReservation>,
    control: ProductionControl<'_>,
) -> T {
    let (value, reservation) = value.into_parts();
    *memory = control.combine(memory.take(), reservation);
    value
}

impl StoredColumnStats<'_> {
    /// Load an ordinary catalog row with its original serde diagnostics and validation order.
    pub fn decode(self) -> Result<ColumnStats> {
        Ok(self
            .read(Decoder::Ordinary)?
            .into_uncontrolled()
            .expect("ordinary stored distribution"))
    }

    /// Admit every owned decoded payload before allocation, retaining both the input owner's and invoking reader's cancellation scopes.
    pub fn decode_with_control(
        self,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Budgeted<ColumnStats>> {
        Ok(self
            .read(Decoder::Controlled {
                memory,
                original,
                invoking,
            })?
            .into_budgeted()
            .expect("admitted stored distribution"))
    }

    fn count(self, value: i64, field: &str) -> Result<u64> {
        value.try_into().map_err(|_| {
            StoredColumnStatsError::Invalid(format!(
                "negative {field} for column `{}`",
                self.column_name
            ))
        })
    }

    fn read(self, decoder: Decoder<'_>) -> Result<Produced<ColumnStats>> {
        let control = decoder.control();
        control.check()?;
        let mut output = Distribution {
            value: ColumnStats::default(),
            memory: control.empty_reservation(),
        };
        let mut histogram = ProductionVec::new(control);
        if self.histogram_json.len() <= limits::encoded_list_bytes(limits::HISTOGRAM_VALUES) {
            let values = decoder.values(self.histogram_json, limits::HISTOGRAM_VALUES)?;
            if values.len() <= limits::HISTOGRAM_VALUES {
                for value in values {
                    if limits::accepts_with_control(&value, &control)? {
                        histogram.push_produced(value)?;
                    }
                }
            }
        }
        output.value.histogram = take(histogram.finish()?, &mut output.memory, control);
        let mut common = ProductionVec::new(control);
        let mut frequencies = ProductionVec::new(control);
        if self.mcv_values_json.len() <= limits::encoded_list_bytes(limits::MCV_VALUES)
            && self.mcv_frequencies_json.len() <= limits::MCV_VALUES * 64
        {
            let values = decoder.values(self.mcv_values_json, limits::MCV_VALUES)?;
            // Frequency syntax is validated even when an oversized value list is discarded.
            let counts = decoder.frequencies(self.mcv_frequencies_json)?;
            if values.len() <= limits::MCV_VALUES {
                if values.len() != counts.len() {
                    return Err(StoredColumnStatsError::Invalid(format!(
                        "mismatched MCV values and frequencies for column `{}`",
                        self.column_name
                    )));
                }
                for (value, count) in values.zip(counts.iter()) {
                    if limits::accepts_with_control(&value, &control)? {
                        common.push_produced(value)?;
                        frequencies.push_copy(*count)?;
                    }
                }
            }
        }
        output.value.mcv_values = take(common.finish()?, &mut output.memory, control);
        output.value.mcv_frequencies = take(frequencies.finish()?, &mut output.memory, control);
        output.value.distinct_count = self.count(self.distinct_count, "distinct_count")?;
        output.value.null_count = self.count(self.null_count, "null_count")?;
        output.value.min_value = take(
            decoder.optional(self.min_value)?,
            &mut output.memory,
            control,
        );
        output.value.max_value = take(
            decoder.optional(self.max_value)?,
            &mut output.memory,
            control,
        );
        output.value.row_count = self.count(self.row_count, "row_count")?;
        Ok(control.finish(output.value, output.memory)?)
    }
}

/// Consume stored rows directly into the native immutable statistics generation. Each call borrows only its current provider row; published values retain their original allowance.
pub struct StoredColumnStatistics<'a> {
    columns: Builder<'a>,
    decoder: Decoder<'a>,
}

impl<'a> StoredColumnStatistics<'a> {
    pub fn new(
        memory: &'a MemoryBudget,
        original: &'a CancellationToken,
        invoking: &'a CancellationToken,
    ) -> Self {
        let decoder = Decoder::Controlled {
            memory,
            original,
            invoking,
        };
        Self {
            columns: Builder::new(decoder.control()),
            decoder,
        }
    }

    pub fn insert(&mut self, row: StoredColumnStats<'_>) -> Result<()> {
        let stats = row.read(self.decoder)?;
        self.columns.insert(row.column_name, stats)?;
        Ok(())
    }

    pub fn finish(self) -> Result<ColumnStatistics> {
        Ok(self.columns.finish()?)
    }
}
