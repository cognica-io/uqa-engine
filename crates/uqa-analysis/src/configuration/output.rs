//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical output borrows native fields and admits the complete destination before copying.

use std::io::Cursor;
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryError},
};

use super::view::{Array, Form, Object, View};
use crate::{AnalysisError, AnalysisResult, Analyzer};

const FRAGMENT: usize = 256;

enum Frame<'a> {
    Value(View<'a>),
    Object(Object<'a>, bool),
    Array(Array<'a>, bool),
}

enum Output {
    Length(usize),
    Bytes(BudgetedVec<u8>),
}

impl Output {
    fn append(&mut self, bytes: &[u8]) -> AnalysisResult<()> {
        match self {
            Self::Length(length) => {
                *length = length
                    .checked_add(bytes.len())
                    .ok_or(MemoryError::SizeOverflow)?;
            }
            Self::Bytes(output) => output.extend_from_slice(bytes)?,
        }
        Ok(())
    }

    async fn text(
        &mut self,
        mut bytes: &[u8],
        input: &mut AsyncImageInput<'_>,
    ) -> AnalysisResult<()> {
        self.append(b"\"")?;
        while !bytes.is_empty() {
            input.checkpoint().await?;
            let fragment = text_fragment(bytes)?;
            self.fragment(fragment)?;
            bytes = &bytes[fragment.len()..];
        }
        self.append(b"\"")
    }

    // The public serializer owns escaping. Each UTF-8 byte expands to at most six
    // bytes; the two enclosing quotes are omitted between adjacent fragments.
    fn fragment(&mut self, text: &str) -> AnalysisResult<()> {
        let mut encoded = [0; FRAGMENT * 6 + 2];
        let mut writer = Cursor::new(encoded.as_mut_slice());
        serde_json::to_writer(&mut writer, text)?;
        let length = usize::try_from(writer.position()).map_err(|_| MemoryError::SizeOverflow)?;
        self.append(&encoded[1..length - 1])
    }
}

fn text_fragment(bytes: &[u8]) -> AnalysisResult<&str> {
    let fragment = &bytes[..bytes.len().min(FRAGMENT)];
    match std::str::from_utf8(fragment) {
        Ok(value) => Ok(value),
        Err(error) if error.error_len().is_none() && error.valid_up_to() != 0 => {
            Ok(std::str::from_utf8(&fragment[..error.valid_up_to()])
                .expect("validated UTF-8 prefix"))
        }
        Err(_) => Err(invalid_path()),
    }
}

pub(super) fn invalid_path() -> AnalysisError {
    <serde_json::Error as serde::ser::Error>::custom("path contains invalid UTF-8 characters")
        .into()
}

async fn write(
    value: &Analyzer,
    output: &mut Output,
    input: &mut AsyncImageInput<'_>,
) -> AnalysisResult<()> {
    let mut stack = BudgetedVec::new(input.memory);
    stack.push(Frame::Value(View::Analyzer(value)))?;
    while let Some(frame) = stack.pop() {
        input.checkpoint().await?;
        match frame {
            Frame::Value(value) => match value.form(false) {
                Form::Object(values) => {
                    output.append(b"{")?;
                    stack.push(Frame::Object(values, true))?;
                }
                Form::Array(values) => {
                    output.append(b"[")?;
                    stack.push(Frame::Array(values, true))?;
                }
                Form::Text(value) => output.text(value.as_bytes(), input).await?,
                Form::Path(value) => {
                    output
                        .text(value.as_os_str().as_encoded_bytes(), input)
                        .await?;
                }
                Form::Scalar(value) => {
                    // Closed scalar enums, booleans and integer fields fit this fixed buffer.
                    let mut encoded = [0; 64];
                    let mut writer = Cursor::new(encoded.as_mut_slice());
                    serde_json::to_writer(&mut writer, &value)?;
                    let length = usize::try_from(writer.position())
                        .map_err(|_| MemoryError::SizeOverflow)?;
                    output.append(&encoded[..length])?;
                }
            },
            Frame::Object(mut values, first) => {
                if let Some((key, value)) = values.next() {
                    if !first {
                        output.append(b",")?;
                    }
                    output.text(key.as_bytes(), input).await?;
                    output.append(b":")?;
                    stack.push(Frame::Object(values, false))?;
                    stack.push(Frame::Value(value))?;
                } else {
                    output.append(b"}")?;
                }
            }
            Frame::Array(mut values, first) => {
                if let Some(value) = values.next() {
                    if !first {
                        output.append(b",")?;
                    }
                    stack.push(Frame::Array(values, false))?;
                    stack.push(Frame::Value(value))?;
                } else {
                    output.append(b"]")?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn encode_from(
    value: &Analyzer,
    mut input: AsyncImageInput<'_>,
) -> AnalysisResult<Budgeted<Vec<u8>>> {
    let mut length = Output::Length(0);
    write(value, &mut length, &mut input).await?;
    let Output::Length(length) = length else {
        unreachable!("size-only output")
    };
    let mut bytes = BudgetedVec::new(input.memory);
    bytes.reserve(length)?;
    let mut output = Output::Bytes(bytes);
    write(value, &mut output, &mut input).await?;
    let Output::Bytes(bytes) = output else {
        unreachable!("admitted output")
    };
    debug_assert_eq!(bytes.len(), length);
    let (bytes, memory) = bytes.into_parts();
    Ok(Budgeted::new(bytes, memory))
}
