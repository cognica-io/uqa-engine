//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    invalid, overflow, Buffer, ColumnObservationCounts, ColumnStats, Ordering, Produced,
    ProductionControl, ProductionVec, SQLError, Value,
};

pub(super) struct Distribution<'a> {
    row_count: u64,
    distinct_count: u64,
    min: Option<Produced<Value>>,
    max: Option<Produced<Value>>,
    histogram: ProductionVec<'a, Value>,
    counts: ColumnObservationCounts,
    group: Option<(Produced<Value>, u64)>,
    common: Buffer<(Produced<Value>, u64)>,
    common_limit: usize,
    comparable_seen: u64,
    retained_seen: u64,
    bucket_count: u64,
    next_bucket: u64,
    control: ProductionControl<'a>,
}

impl<'a> Distribution<'a> {
    pub(super) fn new(
        row_count: u64,
        counts: ColumnObservationCounts,
        histogram_values: usize,
        common_limit: usize,
        control: ProductionControl<'a>,
    ) -> Self {
        Self {
            row_count,
            distinct_count: counts.omitted,
            min: None,
            max: None,
            histogram: ProductionVec::new(control),
            counts,
            group: None,
            common: Buffer::new(control),
            common_limit,
            comparable_seen: 0,
            retained_seen: 0,
            bucket_count: counts.comparable.min((histogram_values - 1) as u64),
            next_bucket: 1,
            control,
        }
    }

    pub(super) fn observe(&mut self, value: &Value) -> Result<(), SQLError> {
        self.control.check()?;
        self.retained_seen = self.retained_seen.checked_add(1).ok_or_else(overflow)?;
        if matches!(
            value,
            Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_)
        ) {
            self.observe_comparable(value)?;
        }
        if let Some((previous, count)) = self.group.as_mut() {
            match previous.cmp_with_control(value, &self.control)? {
                Ordering::Greater => return Err(invalid("ANALYZE values are not ordered")),
                Ordering::Equal => {
                    *count = count.checked_add(1).ok_or_else(overflow)?;
                    return Ok(());
                }
                Ordering::Less => self.finish_group()?,
            }
        }
        self.group = Some((self.control.copy_value(value)?, 1));
        Ok(())
    }

    fn observe_comparable(&mut self, value: &Value) -> Result<(), SQLError> {
        if self.comparable_seen == 0 {
            self.min = Some(self.control.copy_value(value)?);
            self.histogram
                .push_produced(self.control.copy_value(value)?)?;
        } else if self.bucket_count > 1
            && self.next_bucket < self.bucket_count
            && u128::from(self.comparable_seen)
                == u128::from(self.next_bucket) * u128::from(self.counts.comparable)
                    / u128::from(self.bucket_count)
        {
            if !self.last_boundary_equals(value)? {
                self.histogram
                    .push_produced(self.control.copy_value(value)?)?;
            }
            self.next_bucket += 1;
        }
        // The old maximum's payload and lease retire together when the new copy replaces it.
        self.max = Some(self.control.copy_value(value)?);
        self.comparable_seen += 1;
        Ok(())
    }

    fn last_boundary_equals(&self, value: &Value) -> Result<bool, SQLError> {
        self.histogram.last().map_or(Ok(false), |last| {
            Ok(last.cmp_with_control(value, &self.control)? == Ordering::Equal)
        })
    }

    fn finish_group(&mut self) -> Result<(), SQLError> {
        let Some(group) = self.group.take() else {
            return Ok(());
        };
        self.distinct_count = self.distinct_count.checked_add(1).ok_or_else(overflow)?;
        let (mut lower, mut upper) = (0, self.common.len());
        while lower < upper {
            self.control.check()?;
            let middle = lower + (upper - lower) / 2;
            let (value, count) = &self.common[middle];
            if *count > group.1
                || (*count == group.1
                    && value.cmp_with_control(&group.0, &self.control)? == Ordering::Less)
            {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        if lower < self.common_limit {
            // Evict the losing owner before insertion, without growing beyond the retained candidate limit.
            if self.common.len() == self.common_limit {
                self.common.pop();
            }
            self.common.push(group)?;
            self.common[lower..].rotate_right(1);
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<Produced<ColumnStats>, SQLError> {
        self.finish_group()?;
        let retained = self
            .row_count
            .checked_sub(self.counts.nulls)
            .and_then(|count| count.checked_sub(self.counts.omitted))
            .ok_or_else(|| invalid("invalid ANALYZE observation counts"))?;
        if retained != self.retained_seen || self.counts.comparable != self.comparable_seen {
            return Err(invalid("incomplete ANALYZE observations"));
        }
        if let Some(last) = self.max.as_ref() {
            if self.comparable_seen == 1 || !self.last_boundary_equals(last)? {
                self.histogram
                    .push_produced(self.control.copy_value(last)?)?;
            }
        }
        let mut values = ProductionVec::new(self.control);
        let mut frequencies = ProductionVec::new(self.control);
        if self.distinct_count != 0 && self.row_count != 0 {
            let average = 1.0 / self.distinct_count as f64;
            for (value, count) in self.common {
                self.control.check()?;
                let frequency = count as f64 / self.row_count as f64;
                if frequency > average {
                    values.push_produced(value)?;
                    frequencies.push_copy(frequency)?;
                }
            }
        }
        let histogram = self.histogram.finish()?;
        let values = values.finish()?;
        let frequencies = frequencies.finish()?;
        let (histogram, mut memory) = histogram.into_parts();
        let (mcv_values, owned) = values.into_parts();
        memory = self.control.combine(memory, owned);
        let (mcv_frequencies, owned) = frequencies.into_parts();
        memory = self.control.combine(memory, owned);
        let min_value = self.min.map(|value| {
            let (value, owned) = value.into_parts();
            memory = self.control.combine(memory.take(), owned);
            value
        });
        let max_value = self.max.map(|value| {
            let (value, owned) = value.into_parts();
            memory = self.control.combine(memory.take(), owned);
            value
        });
        Ok(self.control.finish(
            ColumnStats {
                row_count: self.row_count,
                null_count: self.counts.nulls,
                distinct_count: self.distinct_count,
                min_value,
                max_value,
                histogram,
                mcv_values,
                mcv_frequencies,
            },
            memory,
        )?)
    }
}
