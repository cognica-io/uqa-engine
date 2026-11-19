//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{limits, Result};
use uqa_core::{
    json::{JsonReadError, JsonReader, JsonToken},
    memory::{
        Budgeted, BudgetedVec, MemoryBudget, MemoryReservation, Produced, ProductionControl,
        ProductionVec,
    },
    CancellationToken, JsonValueDecoder, Value,
};

#[derive(Clone, Copy)]
pub(super) enum Decoder<'a> {
    Ordinary,
    Controlled {
        memory: &'a MemoryBudget,
        original: &'a CancellationToken,
        invoking: &'a CancellationToken,
    },
}

/// The iterator owns its buffer until all remaining values have been dropped.
enum Values {
    Ordinary(std::vec::IntoIter<Value>),
    Controlled {
        values: std::vec::IntoIter<Budgeted<Value>>,
        _memory: MemoryReservation,
    },
}

pub(super) struct ValueList {
    values: Values,
    count: usize,
}

impl ValueList {
    pub(super) fn len(&self) -> usize {
        self.count
    }
}

impl Iterator for ValueList {
    type Item = Produced<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.values {
            Values::Ordinary(values) => values.next().map(|value| {
                ProductionControl::uncontrolled()
                    .finish(value, None)
                    .expect("ordinary stored value")
            }),
            Values::Controlled { values, .. } => values.next().map(Produced::from),
        }
    }
}

impl<'a> Decoder<'a> {
    pub(super) fn control(self) -> ProductionControl<'a> {
        match self {
            Self::Ordinary => ProductionControl::uncontrolled(),
            Self::Controlled {
                memory,
                original,
                invoking,
            } => ProductionControl::new(memory, original, invoking),
        }
    }

    fn value(self, raw: &str, depth: usize) -> Result<Produced<Value>> {
        match self {
            Self::Ordinary => Ok(self.control().finish(serde_json::from_str(raw)?, None)?),
            Self::Controlled {
                memory,
                original,
                invoking,
            } => Ok(JsonValueDecoder::new(memory, original)
                .with_invoking_cancellation(invoking)
                .with_depth_limit(depth)
                .value(raw)?
                .into()),
        }
    }

    pub(super) fn optional(self, raw: Option<&str>) -> Result<Produced<Option<Value>>> {
        let control = self.control();
        control.check()?;
        if let Some(raw) = raw.filter(|text| text.len() <= limits::ENCODED_VALUE_BYTES) {
            let value = self.value(raw, 127)?;
            if !matches!(*value, Value::Null) && limits::accepts_with_control(&value, &control)? {
                let (value, memory) = value.into_parts();
                return Ok(control.finish(Some(value), memory)?);
            }
        }
        Ok(control.finish(None, control.empty_reservation())?)
    }

    pub(super) fn values(self, raw: &str, limit: usize) -> Result<ValueList> {
        let Self::Controlled { memory, .. } = self else {
            let values: Vec<Value> = serde_json::from_str(raw)?;
            return Ok(ValueList {
                count: values.len(),
                values: Values::Ordinary(values.into_iter()),
            });
        };
        let mut values = BudgetedVec::new(memory);
        let mut count = 0;
        array(raw, self.control(), |raw| {
            let value = self
                .value(raw, 126)?
                .into_budgeted()
                .expect("admitted list item");
            count += 1;
            if count <= limit {
                values.push(value).map_err(JsonReadError::from)?;
            } else if count == limit + 1 {
                // Keep validating subsequent values without retaining a discarded population.
                values = BudgetedVec::new(memory);
            }
            Ok(())
        })?;
        let (values, memory) = values.into_parts();
        Ok(ValueList {
            count,
            values: Values::Controlled {
                values: values.into_iter(),
                _memory: memory,
            },
        })
    }

    pub(super) fn frequencies(self, raw: &str) -> Result<Produced<Vec<f64>>> {
        let control = self.control();
        if matches!(self, Self::Ordinary) {
            return Ok(control.finish(serde_json::from_str(raw)?, None)?);
        }
        let mut values = ProductionVec::new(control);
        array(raw, control, |raw| {
            // Use serde's original floating-point conversion, including its exact rounding.
            values.push_copy(serde_json::from_str::<f64>(raw)?)?;
            Ok(())
        })?;
        Ok(values.finish()?)
    }
}

/// Project complete array elements as borrowed slices; parser scratch is admitted independently of the values each visitor constructs.
fn array(
    raw: &str,
    control: ProductionControl<'_>,
    mut visit: impl FnMut(&str) -> Result<()>,
) -> Result<()> {
    let mut input = JsonReader::with_control(raw, &control).with_depth_limit(127);
    if input
        .next_event()?
        .is_none_or(|event| event.token != JsonToken::StartArray)
    {
        return Err(JsonReadError::InvalidJson.into());
    }
    loop {
        let event = input.next_event()?.ok_or(JsonReadError::InvalidJson)?;
        if event.token == JsonToken::EndArray {
            if input.next_event()?.is_some() {
                return Err(JsonReadError::InvalidJson.into());
            }
            return Ok(());
        }
        let start = event.range.start;
        let mut end = event.range.end;
        let mut depth = usize::from(matches!(
            event.token,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            let nested = input.next_event()?.ok_or(JsonReadError::InvalidJson)?;
            match nested.token {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => {}
            }
            end = nested.range.end;
        }
        visit(&raw[start..end])?;
    }
}
