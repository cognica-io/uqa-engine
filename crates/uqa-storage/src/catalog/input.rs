//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog row input retains one provider view and consumes one merged record slot.

use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec};

pub mod index;
pub(crate) mod metadata;
pub(crate) mod relation_security;
pub mod statistics;
pub(crate) mod strings;
pub mod table;

#[derive(Clone, Copy, Debug)]
pub struct CatalogInputLimits {
    pub maximum_key_bytes: usize,
    pub maximum_value_bytes: usize,
}

impl CatalogInputLimits {
    /// Reject an empty physical record window before opening its provider boundary.
    pub fn validate(self) -> StorageBackendResult<()> {
        if self.maximum_key_bytes == 0 || self.maximum_value_bytes == 0 {
            return Err(invalid("zero catalog record limit"));
        }
        Ok(())
    }
}

/// A tombstone or a provider-owned absent text field consumes one slot without returning a row.
#[derive(Debug)]
pub enum CatalogInputStep<T> {
    Pending,
    Row(Budgeted<T>),
    Complete,
}

/// Decode the selected physical representation with its existing catalog predicates. The callback cannot reenter persistence or invoke user code.
pub type CatalogRowDecoder<T> =
    fn(&[u8], &[u8], &StorageReadControl) -> StorageBackendResult<Option<Budgeted<T>>>;

/// Select a live key before reading its value. The selection preserves original physical key validation and cannot reenter persistence.
pub type CatalogKeyFilter = fn(&[u8], &StorageReadControl) -> StorageBackendResult<bool>;
pub type CatalogKeyPrefixFilter =
    fn(&[u8], &str, &StorageReadControl) -> StorageBackendResult<bool>;

struct KeyPrefix {
    prefix: BudgetedString,
    select: CatalogKeyPrefixFilter,
}

pub struct CatalogInput<T> {
    state: State<T>,
}

enum State<T> {
    Reading(Budgeted<Box<Input<T>>>),
    Complete,
    Failed,
}

struct Input<T> {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    prefix: BudgetedVec<u8>,
    after: Option<BudgetedVec<u8>>,
    decode: CatalogRowDecoder<T>,
    select: Option<CatalogKeyFilter>,
    key_prefix: Option<KeyPrefix>,
    limits: CatalogInputLimits,
    control: StorageReadControl,
}

impl<T> CatalogInput<T> {
    pub(crate) fn shares_context(&self, control: &StorageReadControl) -> bool {
        match &self.state {
            State::Reading(input) => input.control.shares_context(control),
            State::Complete => true,
            State::Failed => false,
        }
    }

    /// Complete an absent catalog owner without retaining a provider view or allocating a cursor.
    pub fn empty(
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        Ok(Self {
            state: State::Complete,
        })
    }

    /// Retain an already selected native view without enumerating records. Versioned providers supply the same committed/private view to key selection and bounded value reads. Physical key order is preserved; consumers that require name order retain and order the complete decoded catalog in their owning layer.
    pub fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::create(read, prefix, None, decode, limits, control)
    }

    /// Retain the same bounded stream while rejecting unrelated keys before any value read. Each rejected live key still consumes its one physical slot.
    pub fn new_filtered(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        select: CatalogKeyFilter,
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::create(read, prefix, Some(select), decode, limits, control)
    }

    /// Filter a decoded key prefix before reading a value. The original provider order and one-slot polling are retained, and the prefix allocation belongs to the cursor's original allowance.
    pub fn new_prefix_filtered(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        selection: (&str, CatalogKeyPrefixFilter),
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check_value_size(selection.0.len(), limits.maximum_key_bytes)?;
        let mut selected = BudgetedString::new(control.memory());
        selected.push_str(selection.0)?;
        let mut input = Self::create(read, prefix, None, decode, limits, control)?;
        let State::Reading(value) = &mut input.state else {
            unreachable!("new catalog cursor");
        };
        value.as_mut().key_prefix = Some(KeyPrefix {
            prefix: selected,
            select: selection.1,
        });
        Ok(input)
    }

    fn create(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        select: Option<CatalogKeyFilter>,
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        read.control().check()?;
        limits.validate()?;
        control.check_value_size(prefix.len(), limits.maximum_key_bytes)?;
        let memory = control.memory().reserve(size_of::<Input<T>>())?;
        let mut owned_prefix = BudgetedVec::new(control.memory());
        owned_prefix.extend_from_slice(prefix)?;
        Ok(Self {
            state: State::Reading(Budgeted::new(
                Box::new(Input {
                    read,
                    prefix: owned_prefix,
                    after: None,
                    decode,
                    select,
                    key_prefix: None,
                    limits,
                    control: control.clone(),
                }),
                memory,
            )),
        })
    }

    /// Consume at most one merged record slot, including tombstones, and at most one size-checked value. Errors and unwinds retire the original view and position; a failed input cannot resume. Completion closes the view before returning and remains idempotent.
    pub fn advance(&mut self) -> StorageBackendResult<CatalogInputStep<T>> {
        match self.state {
            State::Complete => return Ok(CatalogInputStep::Complete),
            State::Failed => return Err(invalid("catalog record input is finished")),
            State::Reading(_) => (),
        }
        let State::Reading(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        let step = input.as_mut().advance()?;
        self.state = if matches!(step, CatalogInputStep::Complete) {
            drop(input);
            State::Complete
        } else {
            State::Reading(input)
        };
        Ok(step)
    }
}

impl<T> Input<T> {
    fn advance(&mut self) -> StorageBackendResult<CatalogInputStep<T>> {
        self.control.check()?;
        self.read.control().check()?;
        let mut next = None;
        self.read.visit_record_keys_after(
            &self.prefix,
            self.after.as_deref(),
            1,
            &self.control,
            &mut |key, live| {
                self.control
                    .check_value_size(key.len(), self.limits.maximum_key_bytes)?;
                let mut owned = BudgetedVec::new(self.control.memory());
                owned.extend_from_slice(key)?;
                next = Some((owned, live));
                Ok(())
            },
        )?;
        let Some((key, live)) = next else {
            self.control.check()?;
            self.read.control().check()?;
            return Ok(CatalogInputStep::Complete);
        };
        self.after = Some(key);
        let mut row = None;
        let key = self.after.as_deref().expect("selected record key");
        let selected = live
            && self
                .select
                .map_or(Ok(true), |select| select(key, &self.control))?
            && self.key_prefix.as_ref().map_or(Ok(true), |filter| {
                (filter.select)(key, &filter.prefix, &self.control)
            })?;
        if selected {
            self.read.visit_value_bounded(
                key,
                self.limits.maximum_value_bytes,
                &self.control,
                &mut |value| {
                    let value =
                        value.ok_or_else(|| invalid("selected catalog record is absent"))?;
                    row = (self.decode)(key, value, &self.control)?;
                    Ok(())
                },
            )?;
        }
        self.control.check()?;
        self.read.control().check()?;
        Ok(row.map_or(CatalogInputStep::Pending, CatalogInputStep::Row))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
