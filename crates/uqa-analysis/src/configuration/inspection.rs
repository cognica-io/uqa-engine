//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained size validation and exact native-field observation share borrowed views.

use serde_json::Value;

use super::view::{Array, Form, Object, View};
use crate::{work::Work, AnalysisError, AnalysisResult, Analyzer};

struct Size {
    bytes: usize,
    maximum: usize,
}

impl Size {
    fn add(&mut self, bytes: usize) -> AnalysisResult<()> {
        let required = self
            .bytes
            .checked_add(bytes)
            .ok_or(AnalysisError::Descriptor("analyzer size overflow"))?;
        crate::descriptor::limits::check_limit(
            "analyzer descriptor bytes",
            required,
            self.maximum,
        )?;
        self.bytes = required;
        Ok(())
    }

    async fn string(
        &mut self,
        value: &str,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<()> {
        self.add(1)?;
        let mut start = 0;
        for (chunk, bytes) in value.as_bytes().chunks(4096).enumerate() {
            work.checkpoint().await?;
            for (offset, byte) in bytes.iter().enumerate() {
                let escaped = match *byte {
                    b'"' | b'\\' | b'\x08' | b'\t' | b'\n' | b'\x0c' | b'\r' => 2,
                    0..=31 => 6,
                    _ => continue,
                };
                let offset = chunk * 4096 + offset;
                // Keep the public compact serializer's write boundaries for limit diagnostics.
                self.add(offset - start)?;
                self.add(escaped)?;
                start = offset + 1;
            }
        }
        self.add(value.len() - start)?;
        self.add(1)
    }
}

enum Encoding<'a> {
    Value(View<'a>),
    Object(Object<'a>, bool),
    Array(Array<'a>, bool),
}

pub(crate) async fn check_encoded_size(
    value: &Analyzer,
    maximum: usize,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<()> {
    let mut size = Size { bytes: 0, maximum };
    let mut stack = vec![Encoding::Value(View::Analyzer(value))];
    while let Some(frame) = stack.pop() {
        work.checkpoint().await?;
        match frame {
            Encoding::Value(value) => match value.form(false)? {
                Form::Object(values) => {
                    size.add(1)?;
                    stack.push(Encoding::Object(values, true));
                }
                Form::Array(values) => {
                    size.add(1)?;
                    stack.push(Encoding::Array(values, true));
                }
                Form::Text(value) => size.string(value, work).await?,
                Form::Scalar(Value::String(value)) => size.string(&value, work).await?,
                Form::Scalar(value) => size.add(serde_json::to_vec(&value)?.len())?,
            },
            Encoding::Object(mut values, first) => {
                if let Some((key, value)) = values.next() {
                    if !first {
                        size.add(1)?;
                    }
                    size.string(key, work).await?;
                    size.add(1)?;
                    stack.push(Encoding::Object(values, false));
                    stack.push(Encoding::Value(value));
                } else {
                    size.add(1)?;
                }
            }
            Encoding::Array(mut values, first) => {
                if let Some(value) = values.next() {
                    if !first {
                        size.add(1)?;
                    }
                    stack.push(Encoding::Array(values, false));
                    stack.push(Encoding::Value(value));
                } else {
                    size.add(1)?;
                }
            }
        }
    }
    Ok(())
}

enum Comparison<'a> {
    Value(View<'a>, &'a Value),
    Object(Object<'a>, &'a serde_json::Map<String, Value>),
    Array(Array<'a>, std::slice::Iter<'a, Value>),
}

/// Compare explicit snapshot fields after native canonical-set validation.
pub(crate) async fn matches_snapshot(
    config: &Analyzer,
    value: &Value,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<bool> {
    let mut stack = vec![Comparison::Value(View::Analyzer(config), value)];
    while let Some(frame) = stack.pop() {
        work.checkpoint().await?;
        match frame {
            Comparison::Value(expected, actual) => match (expected.form(true)?, actual) {
                (Form::Object(values), Value::Object(actual)) if values.len() == actual.len() => {
                    stack.push(Comparison::Object(values, actual));
                }
                (Form::Array(values), Value::Array(actual)) if values.len() == actual.len() => {
                    stack.push(Comparison::Array(values, actual.iter()));
                }
                (Form::Text(expected), Value::String(actual)) => {
                    if !crate::json::bytes_equal(expected.as_bytes(), actual.as_bytes(), work)
                        .await?
                    {
                        return Ok(false);
                    }
                }
                (Form::Scalar(expected), actual) if expected == *actual => (),
                _ => return Ok(false),
            },
            Comparison::Object(mut values, actual) => {
                if let Some((key, value)) = values.next() {
                    let Some(actual_value) = actual.get(key) else {
                        return Ok(false);
                    };
                    stack.push(Comparison::Object(values, actual));
                    stack.push(Comparison::Value(value, actual_value));
                }
            }
            Comparison::Array(mut values, mut actual) => {
                if let Some(value) = values.next() {
                    let actual_value = actual.next().expect("equal-length native field arrays");
                    stack.push(Comparison::Array(values, actual));
                    stack.push(Comparison::Value(value, actual_value));
                }
            }
        }
    }
    Ok(true)
}
