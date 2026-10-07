//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command index names and canonical bytes retain the original allowance.

use super::{resource_error, CommandStoredDocument, Document, SQLError, StorageReadControl, Value};
use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::memory::{BudgetedString, BudgetedVec, MemoryReservation};

pub(super) struct FieldSet {
    values: Vec<String>,
    _memory: MemoryReservation,
}

/// Physical tuple names and user columns occupy distinct index namespaces.
#[derive(Clone, Copy)]
pub(super) enum KeyKind {
    Columns,
    Expressions,
}

impl KeyKind {
    pub(super) fn document(self, row: &CommandStoredDocument) -> Result<&Document, SQLError> {
        match self {
            Self::Columns => Ok(row.fields.as_ref()),
            Self::Expressions => row
                .index_values
                .as_deref()
                .ok_or_else(|| SQLError::Internal("staged row lacks evaluated index keys".into())),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct KeyFields<'a> {
    pub(super) kind: KeyKind,
    pub(super) fields: &'a FieldSet,
}

impl KeyFields<'_> {
    pub(super) fn complete(self, row: &CommandStoredDocument) -> Result<bool, SQLError> {
        let document = self.kind.document(row)?;
        Ok(self
            .fields
            .values()
            .iter()
            .all(|field| document.contains_key(field)))
    }
}

impl FieldSet {
    pub(super) fn copy<'a>(
        fields: impl Iterator<Item = &'a str>,
        control: &StorageReadControl,
    ) -> Result<Self, SQLError> {
        let mut memory = control.memory().empty_reservation();
        let mut values = BudgetedVec::new(control.memory());
        for field in fields {
            control.check().map_err(resource_error)?;
            let mut name = BudgetedString::new(control.memory());
            name.push_str(field).map_err(resource_error)?;
            let (name, retained) = name.into_parts();
            values.push(name).map_err(resource_error)?;
            memory.absorb(retained);
        }
        let (values, retained) = values.into_parts();
        memory.absorb(retained);
        Ok(Self {
            values,
            _memory: memory,
        })
    }

    pub(super) fn values(&self) -> &[String] {
        &self.values
    }
}

impl Borrow<[String]> for FieldSet {
    fn borrow(&self) -> &[String] {
        &self.values
    }
}

impl PartialEq for FieldSet {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}
impl Eq for FieldSet {}
impl PartialOrd for FieldSet {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for FieldSet {
    fn cmp(&self, other: &Self) -> Ordering {
        self.values.cmp(&other.values)
    }
}

pub(super) struct ExactKey(BudgetedVec<u8>);

impl ExactKey {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Borrow<[u8]> for ExactKey {
    fn borrow(&self) -> &[u8] {
        self.bytes()
    }
}
impl PartialEq for ExactKey {
    fn eq(&self, other: &Self) -> bool {
        self.bytes() == other.bytes()
    }
}
impl Eq for ExactKey {}
impl PartialOrd for ExactKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ExactKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes().cmp(other.bytes())
    }
}

pub(super) fn lookup_parts(
    fields: &[String],
    values: &[Value],
    control: &StorageReadControl,
) -> Result<(FieldSet, ExactKey), SQLError> {
    let mut pairs = BudgetedVec::new(control.memory());
    for (position, (field, value)) in fields.iter().zip(values).enumerate() {
        control.check().map_err(resource_error)?;
        pairs
            .push((position, field.as_str(), value))
            .map_err(resource_error)?;
    }
    uqa_core::ordering::sort_by_with_control(
        &mut pairs,
        &mut || control.check().map_err(resource_error),
        |left, right, _| Ok(left.1.cmp(right.1).then_with(|| left.0.cmp(&right.0))),
    )?;
    let fields = FieldSet::copy(pairs.iter().map(|(_, field, _)| *field), control)?;
    let key = key(pairs.iter().map(|(_, _, value)| Some(*value)), control)?;
    Ok((fields, key))
}

pub(super) fn document_key(
    row: &CommandStoredDocument,
    fields: KeyFields<'_>,
    control: &StorageReadControl,
) -> Result<ExactKey, SQLError> {
    let document = fields.kind.document(row)?;
    key(
        fields.fields.values.iter().map(|field| document.get(field)),
        control,
    )
}

fn key<'a>(
    values: impl ExactSizeIterator<Item = Option<&'a Value>>,
    control: &StorageReadControl,
) -> Result<ExactKey, SQLError> {
    crate::distinct::canonical_row_key_budgeted(values, control)
        .map(ExactKey)
        .map_err(|error| match error {
            crate::ExecError::SQL(error) => error,
            crate::ExecError::Other(error) => SQLError::Internal(error),
        })
}
