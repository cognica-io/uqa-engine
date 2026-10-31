//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Representation comparison retains independent decoded values until both observations finish.

use super::{RetainedConstantValue, RetainedScalarLiteral};
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(crate) enum Observation<'a> {
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

pub(crate) fn observe<'a>(
    value: &'a RetainedConstantValue,
    control: &ProductionControl<'_>,
) -> Result<Option<Observation<'a>>, SQLError> {
    crate::expr::check_statement_cancellation()?;
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

impl RetainedScalarLiteral {
    /// Compare the exact retained value and metadata with admission for independent spilled reads. Semantic read failures conservatively decline equality; resource failures remain observable errors.
    pub fn has_same_representation_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        let value_matches = if self.value.shares_value_with(&other.value) {
            true
        } else {
            // Preserve both independent observations before comparing metadata, even if one semantic read fails.
            let left = observe(&self.value, control)?;
            let right = observe(&other.value, control)?;
            match (left, right) {
                (Some(left), Some(right)) => left.has_same_representation(&right),
                _ => false,
            }
        };
        Ok(value_matches
            && self.ty == other.ty
            && self.declaration == other.declaration
            && self.collation_oid == other.collation_oid)
    }
}
