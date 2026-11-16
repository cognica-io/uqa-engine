//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private value owners use the same allocation-free tree transitions as native syntax and scalar plans.

use crate::{
    memory::{RetirementNode, TreeRetirement},
    RecordDatumField, Value,
};

/// Owned native value retirement. A caller transfers its private value before a fallible work boundary and retains its payload reservation until this owner is destroyed or completes. No value is interpreted or copied.
pub type ValueRetirement = TreeRetirement<Value>;

#[doc(hidden)]
#[derive(Default)]
pub struct ValueMetadata {
    names: Option<Vec<String>>,
    fields: Option<Vec<RecordDatumField>>,
}

fn values(value: &mut Value) -> Option<&mut Vec<Value>> {
    match value {
        Value::List(values) => Some(values),
        Value::Array(value) => Some(value.retirement_values()),
        Value::Row(value) => Some(value.retirement_values()),
        Value::Record(value) => Some(value.retirement_values()),
        Value::LegacyVector(value) => Some(value.retirement_values()),
        _ => None,
    }
}

impl RetirementNode for Value {
    type Local = ValueMetadata;
    fn empty() -> Self {
        Self::Null
    }
    fn first_child(&mut self) -> Option<&mut Self> {
        if let Self::Map(fields) = self {
            fields
                .first_entry()
                .map(std::collections::btree_map::OccupiedEntry::into_mut)
        } else {
            values(self).and_then(|values| values.first_mut())
        }
    }
    fn take_remaining_child(&mut self) -> Option<Self> {
        if let Self::Map(fields) = self {
            (fields.len() > 1).then(|| fields.pop_last().unwrap().1)
        } else {
            let values = values(self)?;
            (values.len() > 1).then(|| values.pop().unwrap())
        }
    }
    fn retire_local(&mut self, local: &mut ValueMetadata) -> bool {
        if let Some(names) = &mut local.names {
            if names.pop().is_none() {
                local.names = None;
            }
            return false;
        }
        if let Some(fields) = &mut local.fields {
            if fields.pop().is_none() {
                local.fields = None;
            }
            return false;
        }
        match self {
            Self::Record(value) => {
                local.names = value.retirement_names().map(<[String]>::into_vec);
                local.names.is_none()
            }
            Self::Datum(value) => {
                local.fields = value.retirement_fields();
                local.fields.is_none()
            }
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests;
