//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime metadata equality retains the existing JSON value representation.

use super::{DictionaryResult, Value, Work};

pub(crate) async fn bytes_equal(
    left: &[u8],
    right: &[u8],
    work: &mut impl Work,
) -> DictionaryResult<bool> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.chunks(4096).zip(right.chunks(4096)) {
        work.checkpoint().await?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

enum Frame<'a> {
    Value(&'a Value, &'a Value),
    Array(std::iter::Zip<std::slice::Iter<'a, Value>, std::slice::Iter<'a, Value>>),
    Object(
        serde_json::map::Iter<'a>,
        &'a serde_json::Map<String, Value>,
    ),
}

pub(crate) async fn equal_with(
    left: &Value,
    right: &Value,
    work: &mut impl Work,
) -> DictionaryResult<bool> {
    let mut stack = vec![Frame::Value(left, right)];
    while let Some(frame) = stack.pop() {
        work.checkpoint().await?;
        match frame {
            Frame::Value(Value::Null, Value::Null) => (),
            Frame::Value(Value::Bool(left), Value::Bool(right)) if left == right => (),
            Frame::Value(Value::Number(left), Value::Number(right)) => {
                if !bytes_equal(left.as_str().as_bytes(), right.as_str().as_bytes(), work).await? {
                    return Ok(false);
                }
            }
            Frame::Value(Value::String(left), Value::String(right)) => {
                if !bytes_equal(left.as_bytes(), right.as_bytes(), work).await? {
                    return Ok(false);
                }
            }
            Frame::Value(Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                stack.push(Frame::Array(left.iter().zip(right)));
            }
            Frame::Value(Value::Object(left), Value::Object(right))
                if left.len() == right.len() =>
            {
                stack.push(Frame::Object(left.iter(), right));
            }
            Frame::Value(_, _) => return Ok(false),
            Frame::Array(mut values) => {
                if let Some((left, right)) = values.next() {
                    stack.push(Frame::Array(values));
                    stack.push(Frame::Value(left, right));
                }
            }
            Frame::Object(mut values, right) => {
                if let Some((key, left)) = values.next() {
                    let Some(value) = right.get(key) else {
                        return Ok(false);
                    };
                    stack.push(Frame::Object(values, right));
                    stack.push(Frame::Value(left, value));
                }
            }
        }
    }
    Ok(true)
}
