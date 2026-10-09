//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compare physical JSONB entries in `PostgreSQL` iterator order, stopping at the first difference.

use super::{
    corrupt,
    walk::{Event, Input, Scalar, Stream},
    ProductionControl, Result,
};
use crate::expr::json::production::parsed;
use std::cmp::Ordering;
use uqa_core::DecimalValue;

pub(in crate::expr) enum JsonbInput<'a> {
    Bytes(&'a [u8]),
    Text(&'a str),
}

pub(in crate::expr) fn compare_jsonb_datums_with_control(
    left: JsonbInput<'_>,
    right: JsonbInput<'_>,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    let left_node = match &left {
        JsonbInput::Text(text) => Some(parsed::parse(text, control)?),
        _ => None,
    };
    let right_node = match &right {
        JsonbInput::Text(text) => Some(parsed::parse(text, control)?),
        _ => None,
    };
    let left = match left {
        JsonbInput::Bytes(bytes) => Input::Bytes(bytes),
        JsonbInput::Text(_) => Input::Node(left_node.as_ref().expect("parsed text")),
    };
    let right = match right {
        JsonbInput::Bytes(bytes) => Input::Bytes(bytes),
        JsonbInput::Text(_) => Input::Node(right_node.as_ref().expect("parsed text")),
    };
    let mut left = Stream::new(left, control)?;
    let mut right = Stream::new(right, control)?;
    loop {
        let left = left.next(control)?;
        let right = right.next(control)?;
        let (Some(left), Some(right)) = (left, right) else {
            return Ok(Ordering::Equal);
        };
        let order = compare_event(left, right, control)?;
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
}

fn rank(event: &Event<'_>) -> u8 {
    match event {
        Event::End(_) | Event::Scalar(Scalar::Null, _) => 0,
        Event::Scalar(Scalar::String(_), _) => 1,
        Event::Scalar(Scalar::Number(_), _) => 2,
        Event::Scalar(Scalar::Bool(_), _) => 3,
        Event::Begin(container, _) if !container.object => 4,
        Event::Begin(_, _) => 5,
    }
}

fn compare_event(
    left: Event<'_>,
    right: Event<'_>,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    let order = rank(&left).cmp(&rank(&right));
    if order != Ordering::Equal {
        return Ok(order);
    }
    Ok(match (left, right) {
        (Event::Begin(left, _), Event::Begin(right, _)) => {
            // PostgreSQL lets the count override scalar-wrapper precedence, including [] < null.
            left.count
                .cmp(&right.count)
                .then_with(|| right.scalar.cmp(&left.scalar))
        }
        (Event::Scalar(Scalar::Bool(left), _), Event::Scalar(Scalar::Bool(right), _)) => {
            left.cmp(&right)
        }
        (Event::Scalar(Scalar::String(left), _), Event::Scalar(Scalar::String(right), _)) => {
            compare_bytes(left.bytes()?, right.bytes()?, control)?
        }
        (Event::Scalar(Scalar::Number(left), _), Event::Scalar(Scalar::Number(right), _)) => {
            let left_text = left.text(control)?;
            let right_text = right.text(control)?;
            let left =
                DecimalValue::parse_with_control(&left_text, control)?.ok_or_else(corrupt)?;
            let right =
                DecimalValue::parse_with_control(&right_text, control)?.ok_or_else(corrupt)?;
            left.cmp_with_control(&right, control)?
        }
        _ => Ordering::Equal,
    })
}

fn compare_bytes(left: &[u8], right: &[u8], control: &ProductionControl<'_>) -> Result<Ordering> {
    let shared = left.len().min(right.len());
    for (left, right) in left[..shared]
        .chunks(4096)
        .zip(right[..shared].chunks(4096))
    {
        control.check()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}
