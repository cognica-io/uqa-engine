//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical native values retain container order and fragment-wise string serialization.

use super::fragment;
use crate::work::Work;
use serde_json::Value;
use std::collections::{btree_map, BTreeMap};

enum Frame<'a> {
    Value(&'a Value),
    Array(std::slice::Iter<'a, Value>, bool),
    Object(btree_map::IntoIter<&'a str, &'a Value>, bool),
}

pub(crate) trait Output<E>: Send {
    fn write_str(&mut self, value: &str) -> Result<(), E>;
}

impl<E> Output<E> for Vec<u8> {
    fn write_str(&mut self, value: &str) -> Result<(), E> {
        self.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

pub(crate) async fn write_value<E: From<serde_json::Error>>(
    value: &Value,
    output: &mut impl Output<E>,
    work: &mut impl Work<E>,
) -> Result<(), E> {
    let mut stack = vec![Frame::Value(value)];
    while let Some(frame) = stack.pop() {
        work.checkpoint().await?;
        match frame {
            Frame::Value(Value::Null) => output.write_str("null")?,
            Frame::Value(Value::Bool(value)) => {
                output.write_str(if *value { "true" } else { "false" })?;
            }
            Frame::Value(Value::Number(value)) => {
                write_raw(value.as_str(), output, work).await?;
            }
            Frame::Value(Value::String(value)) => write_string(value, output, work).await?,
            Frame::Value(Value::Array(values)) => {
                output.write_str("[")?;
                stack.push(Frame::Array(values.iter(), true));
            }
            Frame::Value(Value::Object(values)) => {
                let mut ordered = BTreeMap::new();
                for (index, (key, value)) in values.iter().enumerate() {
                    work.records(index).await?;
                    ordered.insert(key.as_str(), value);
                }
                output.write_str("{")?;
                stack.push(Frame::Object(ordered.into_iter(), true));
            }
            Frame::Array(mut values, first) => {
                if let Some(value) = values.next() {
                    if !first {
                        output.write_str(",")?;
                    }
                    stack.push(Frame::Array(values, false));
                    stack.push(Frame::Value(value));
                } else {
                    output.write_str("]")?;
                }
            }
            Frame::Object(mut values, first) => {
                if let Some((key, value)) = values.next() {
                    if !first {
                        output.write_str(",")?;
                    }
                    write_string(key, output, work).await?;
                    output.write_str(":")?;
                    stack.push(Frame::Object(values, false));
                    stack.push(Frame::Value(value));
                } else {
                    output.write_str("}")?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn write_raw<E>(
    value: &str,
    output: &mut impl Output<E>,
    work: &mut impl Work<E>,
) -> Result<(), E> {
    let mut position = 0;
    while position < value.len() {
        work.checkpoint().await?;
        let part = fragment(value, position);
        output.write_str(part)?;
        position += part.len();
    }
    Ok(())
}

pub(crate) async fn write_string<E: From<serde_json::Error>>(
    value: &str,
    output: &mut impl Output<E>,
    work: &mut impl Work<E>,
) -> Result<(), E> {
    output.write_str("\"")?;
    let mut position = 0;
    let mut encoded = Vec::new();
    while position < value.len() {
        work.checkpoint().await?;
        let part = fragment(value, position);
        encoded.clear();
        serde_json::to_writer(&mut encoded, part).map_err(E::from)?;
        output.write_str(
            std::str::from_utf8(&encoded[1..encoded.len() - 1])
                .expect("the JSON serializer emits valid UTF-8 fragments"),
        )?;
        position += part.len();
    }
    output.write_str("\"")?;
    Ok(())
}
