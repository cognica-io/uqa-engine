//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered maps and positional declarations retain distinct omission boundaries.

use super::Cursor;
use super::{
    iterator, Input, JsonDiagnostic, JsonPosition, JsonReadError, JsonReadWork, JsonValue, Result,
};

type Map<'a> = Cursor<'a, (&'a str, JsonValue<'a>)>;
type Sequence<'a> = Cursor<'a, JsonValue<'a>>;
enum Values<'a> {
    Map(Map<'a>),
    Sequence {
        values: Sequence<'a>,
        consumed: usize,
        length: usize,
    },
}

pub(in crate::ast::query_input) struct Fields<'a> {
    input: Input<'a>,
    names: &'static [&'static str],
    required: [u64; 2],
    seen: u64,
    expected: &'static str,
    name: Option<&'a str>,
    values: Values<'a>,
}

impl<'a> Fields<'a> {
    pub(in crate::ast::query_input) async fn new(
        input: Input<'a>,
        names: &'static [&'static str],
        required: [u64; 2],
        expected: &'static str,
        flattened: bool,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<Self> {
        input.checkpoint(work).await?;
        assert!(
            names.len() <= u64::BITS as usize,
            "fixed stored declaration fields"
        );
        let values = if let Some(fields) = input.value.fields() {
            Values::Map(iterator(input.control, fields)?)
        } else if let Some(values) = input.value.elements().filter(|_| !flattened) {
            let length = values.len();
            Values::Sequence {
                values: iterator(input.control, values)?,
                consumed: 0,
                length,
            }
        } else {
            return Err(input.type_error(expected, work).await?);
        };
        Ok(Self {
            input,
            names,
            required,
            seen: 0,
            expected,
            name: None,
            values,
        })
    }

    pub(in crate::ast::query_input) fn name(&self) -> Option<&'a str> {
        self.name
    }

    /// Unknown fields are returned at their original position so flattened consumers can perform their own generic-content observation.
    pub(in crate::ast::query_input) async fn next(
        &mut self,
        work: &mut impl JsonReadWork<Error = JsonReadError>,
    ) -> Result<Option<(Option<usize>, JsonValue<'a>)>> {
        self.input.checkpoint(work).await?;
        match &mut self.values {
            Values::Map(values) => {
                if let Some((name, value)) = values.next() {
                    self.name = Some(name);
                    let index = self
                        .names
                        .iter()
                        .position(|expected| name == super::super::fields::name(expected));
                    if let Some(index) = index {
                        self.seen |= 1 << index;
                    }
                    Ok(Some((index, value)))
                } else {
                    if let Some(index) = (0..self.names.len()).find(|&index| {
                        self.required[0] & (1 << index) != 0 && self.seen & (1 << index) == 0
                    }) {
                        return Err(JsonDiagnostic::missing_field(
                            super::super::fields::name(self.names[index]),
                            JsonPosition::unlocated(),
                        )
                        .into());
                    }
                    Ok(None)
                }
            }
            Values::Sequence {
                values,
                consumed,
                length,
            } => match values.next() {
                Some(value) if *consumed < self.names.len() => {
                    let index = *consumed;
                    *consumed += 1;
                    Ok(Some((Some(index), value)))
                }
                Some(_) => Err(JsonDiagnostic::invalid_length(
                    *length,
                    "fewer elements in array",
                    JsonPosition::unlocated(),
                )
                .into()),
                None => {
                    if let Some(index) = (*consumed..self.names.len())
                        .find(|&index| self.required[1] & (1 << index) != 0)
                    {
                        return Err(JsonDiagnostic::invalid_field_length(
                            index,
                            self.expected,
                            self.names.len(),
                            JsonPosition::unlocated(),
                        )
                        .into());
                    }
                    Ok(None)
                }
            },
        }
    }
}
