//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained identity keeps both decoded values owned through comparison and preserves conservative semantic-read failure.

use crate::{
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

enum Observation<'a> {
    Resident(&'a Value),
    Decoded(Produced<Value>),
}

impl std::ops::Deref for Observation<'_> {
    type Target = Value;
    fn deref(&self) -> &Value {
        match self {
            Self::Resident(value) => value,
            Self::Decoded(value) => value,
        }
    }
}

fn observe<'a>(
    value: &'a RetainedConstantValue,
    control: &ProductionControl<'_>,
) -> Result<Option<Observation<'a>>, SQLError> {
    control.check()?;
    if let Some(value) = value.resident_value() {
        return Ok(Some(Observation::Resident(value)));
    }
    match value.read_with_control(control) {
        Ok(value) => Ok(Some(Observation::Decoded(value))),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
    }
}

pub(super) fn same(
    left: &RetainedScalarLiteral,
    right: &RetainedScalarLiteral,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let value_matches = if left.value.shares_value_with(&right.value) {
        true
    } else {
        // Both independent reads precede metadata comparison, including when one reports a semantic storage error.
        let left_value = observe(&left.value, control)?;
        let right_value = observe(&right.value, control)?;
        match (left_value, right_value) {
            (Some(left), Some(right)) => super::super::literals_equal(&left, &right),
            _ => false,
        }
    };
    Ok(value_matches
        && left.ty == right.ty
        && left.declaration == right.declaration
        && left.collation_oid == right.collation_oid)
}

pub(super) fn inline(
    literal: &RetainedScalarLiteral,
    other: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Ok(observe(&literal.value, control)?
        .is_some_and(|value| super::retained_value_matches(literal, &value, other)))
}
