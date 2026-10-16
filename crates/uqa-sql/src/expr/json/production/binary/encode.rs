//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build canonical JSONB keys, entry offsets and aligned numeric/container payloads.

use super::{
    aligned, corrupt, ProductionControl, Result, SQLError, ARRAY, CONTAINER, COUNT_MASK, FALSE,
    HAS_OFFSET, NULL, NUMERIC, OBJECT, SCALAR, TRUE,
};
use crate::expr::datums::encoding::{append, pad_to};
use crate::expr::json::production::{parsed, writer, Node};
use uqa_core::{
    memory::{Produced, ProductionVec},
    DecimalValue,
};

pub(super) fn encode(text: &str) -> Result<Vec<u8>> {
    Ok(
        encode_with_control(text, &ProductionControl::uncontrolled())?
            .into_uncontrolled()
            .expect("ordinary JSONB encoding"),
    )
}

pub(super) fn encode_with_control(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>> {
    let node = parsed::parse(text, control)?;
    let mut bytes = ProductionVec::new(*control);
    container(&node, &mut bytes, control)?;
    Ok(bytes.finish()?)
}

fn container(
    node: &Node,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<()> {
    control.check()?;
    let start = bytes.len();
    pad_to(bytes, aligned(start)?)?;
    let (count, flag) = match node {
        Node::Array(values) => (values.len(), ARRAY),
        Node::Object(fields) => (fields.len(), OBJECT),
        _ => (1, ARRAY | SCALAR),
    };
    let count_word = length(count, flag)?;
    append(bytes, &(count_word | flag).to_le_bytes())?;
    let entries = bytes.len();
    let slots = count
        .checked_mul(if flag == OBJECT { 2 } else { 1 })
        .ok_or_else(corrupt)?;
    let data = entries
        .checked_add(slots.checked_mul(4).ok_or_else(corrupt)?)
        .ok_or_else(corrupt)?;
    pad_to(bytes, data)?;
    match node {
        Node::Object(fields) => {
            let ordered = writer::ordered(fields, true, control)?;
            for (index, (_, field)) in ordered.entries.iter().enumerate() {
                let start = bytes.len();
                append(bytes, field.key.as_bytes())?;
                entry(bytes.as_mut_slice(), entries, data, index, start, 0, flag)?;
            }
            for (index, (_, field)) in ordered.entries.iter().enumerate() {
                let start = bytes.len();
                let kind = value(&field.value, bytes, control)?;
                entry(
                    bytes.as_mut_slice(),
                    entries,
                    data,
                    count + index,
                    start,
                    kind,
                    flag,
                )?;
            }
        }
        Node::Array(values) => {
            for (index, node) in values.iter().enumerate() {
                let start = bytes.len();
                let kind = value(node, bytes, control)?;
                entry(
                    bytes.as_mut_slice(),
                    entries,
                    data,
                    index,
                    start,
                    kind,
                    flag,
                )?;
            }
        }
        node => {
            let start = bytes.len();
            let kind = value(node, bytes, control)?;
            entry(bytes.as_mut_slice(), entries, data, 0, start, kind, flag)?;
        }
    }
    length(bytes.len() - start, flag)?;
    Ok(())
}

fn value(
    node: &Node,
    bytes: &mut ProductionVec<'_, u8>,
    control: &ProductionControl<'_>,
) -> Result<u32> {
    control.check()?;
    Ok(match node {
        Node::Null => NULL,
        Node::Bool(false) => FALSE,
        Node::Bool(true) => TRUE,
        Node::String(text) => {
            append(bytes, text.as_bytes())?;
            0
        }
        Node::Number(text) => {
            let number = DecimalValue::parse_with_control(text, control)?.ok_or_else(corrupt)?;
            let payload =
                crate::catalog::node_tree::encode_numeric_datum_with_control(&number, control)?;
            pad_to(bytes, aligned(bytes.len())?)?;
            let total = u32::try_from(payload.len().checked_add(4).ok_or_else(corrupt)?)
                .ok()
                .and_then(|size| size.checked_mul(4))
                .ok_or_else(corrupt)?;
            append(bytes, &total.to_le_bytes())?;
            append(bytes, &payload)?;
            NUMERIC
        }
        Node::Array(_) | Node::Object(_) => {
            container(node, bytes, control)?;
            CONTAINER
        }
    })
}

fn entry(
    bytes: &mut [u8],
    entries: usize,
    data: usize,
    index: usize,
    start: usize,
    kind: u32,
    flag: u32,
) -> Result<()> {
    let end = length(bytes.len() - data, flag)?;
    let entry = kind
        | if index.is_multiple_of(32) {
            HAS_OFFSET | end
        } else {
            length(bytes.len() - start, flag)?
        };
    let offset = entries + index * 4;
    bytes[offset..offset + 4].copy_from_slice(&entry.to_le_bytes());
    Ok(())
}

fn length(size: usize, flag: u32) -> Result<u32> {
    u32::try_from(size)
        .ok()
        .filter(|size| *size <= COUNT_MASK)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "54000".into(),
            message: format!(
                "total size of jsonb {} elements exceeds the maximum of {COUNT_MASK} bytes",
                if flag == OBJECT { "object" } else { "array" }
            ),
        })
}
