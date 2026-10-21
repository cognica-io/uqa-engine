//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact distribution statistics from stable Core-ordered observations, without population-sized maps.

use super::ColumnStats;
use std::cmp::Ordering;
use uqa_core::Value;
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

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
    if histogram_values < 2 {
        return Err(invalid("ANALYZE histogram requires two boundary slots"));
    }
    let mut columns = counts
        .iter()
        .map(|counts| Distribution::new(row_count, counts, histogram_values, mcv_values))
        .collect::<Vec<_>>();
    for item in values {
        let (column, value) = item?;
        columns
            .get_mut(column)
            .ok_or_else(|| invalid("ANALYZE value has an unknown column"))?
            .observe(value)?;
    }
    columns.into_iter().map(Distribution::finish).collect()
}

struct Distribution {
    result: ColumnStats,
    counts: ColumnObservationCounts,
    group: Option<(Value, u64)>,
    common: Vec<(Value, u64)>,
    common_limit: usize,
    comparable_seen: u64,
    retained_seen: u64,
    bucket_count: u64,
    next_bucket: u64,
}

impl Distribution {
    fn new(
        row_count: u64,
        counts: &ColumnObservationCounts,
        histogram_values: usize,
        common_limit: usize,
    ) -> Self {
        Self {
            result: ColumnStats {
                row_count,
                null_count: counts.nulls,
                distinct_count: counts.omitted,
                ..ColumnStats::default()
            },
            counts: ColumnObservationCounts {
                nulls: counts.nulls,
                omitted: counts.omitted,
                comparable: counts.comparable,
            },
            group: None,
            common: Vec::new(),
            common_limit,
            comparable_seen: 0,
            retained_seen: 0,
            bucket_count: counts.comparable.min((histogram_values - 1) as u64),
            next_bucket: 1,
        }
    }

    fn observe(&mut self, value: Value) -> Result<(), SQLError> {
        self.retained_seen = self.retained_seen.checked_add(1).ok_or_else(overflow)?;
        if matches!(
            value,
            Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
        ) {
            self.observe_comparable(&value);
        }
        if let Some((previous, count)) = self.group.as_mut() {
            match (*previous).cmp(&value) {
                Ordering::Greater => return Err(invalid("ANALYZE values are not ordered")),
                Ordering::Equal => {
                    *count = count.checked_add(1).ok_or_else(overflow)?;
                    return Ok(());
                }
                Ordering::Less => self.finish_group()?,
            }
        }
        self.group = Some((value, 1));
        Ok(())
    }

    fn observe_comparable(&mut self, value: &Value) {
        if self.comparable_seen == 0 {
            self.result.min_value = Some(value.clone());
            self.result.histogram.push(value.clone());
        } else if self.bucket_count > 1
            && self.next_bucket < self.bucket_count
            && u128::from(self.comparable_seen)
                == u128::from(self.next_bucket) * u128::from(self.counts.comparable)
                    / u128::from(self.bucket_count)
        {
            if self.result.histogram.last() != Some(value) {
                self.result.histogram.push(value.clone());
            }
            self.next_bucket += 1;
        }
        self.result.max_value = Some(value.clone());
        self.comparable_seen += 1;
    }

    fn finish_group(&mut self) -> Result<(), SQLError> {
        let Some(group) = self.group.take() else {
            return Ok(());
        };
        self.result.distinct_count = self
            .result
            .distinct_count
            .checked_add(1)
            .ok_or_else(overflow)?;
        let position = self.common.partition_point(|(value, count)| {
            *count > group.1 || (*count == group.1 && value < &group.0)
        });
        if position < self.common_limit {
            self.common.insert(position, group);
            self.common.truncate(self.common_limit);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ColumnStats, SQLError> {
        self.finish_group()?;
        let retained = self
            .result
            .row_count
            .checked_sub(self.counts.nulls)
            .and_then(|count| count.checked_sub(self.counts.omitted))
            .ok_or_else(|| invalid("invalid ANALYZE observation counts"))?;
        if retained != self.retained_seen || self.counts.comparable != self.comparable_seen {
            return Err(invalid("incomplete ANALYZE observations"));
        }
        if let Some(last) = self.result.max_value.as_ref() {
            if self.comparable_seen == 1 || self.result.histogram.last() != Some(last) {
                self.result.histogram.push(last.clone());
            }
        }
        if self.result.distinct_count != 0 && self.result.row_count != 0 {
            let average = 1.0 / self.result.distinct_count as f64;
            for (value, count) in self.common {
                let frequency = count as f64 / self.result.row_count as f64;
                if frequency > average {
                    self.result.mcv_values.push(value);
                    self.result.mcv_frequencies.push(frequency);
                }
            }
        }
        Ok(self.result)
    }
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(message.into())
}

fn overflow() -> SQLError {
    invalid("ANALYZE observation count overflow")
}
