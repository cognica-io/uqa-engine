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
use uqa_core::memory::{Budgeted, BudgetedVec};

pub mod index;

#[derive(Clone, Copy, Debug)]
pub struct CatalogInputLimits {
    pub maximum_key_bytes: usize,
    pub maximum_value_bytes: usize,
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
    limits: CatalogInputLimits,
    control: StorageReadControl,
}

impl<T> CatalogInput<T> {
    /// Retain an already selected native view without enumerating records. Versioned providers supply the same committed/private view to key selection and bounded value reads. Physical key order is preserved; consumers that require name order retain and order the complete decoded catalog in their owning layer.
    pub fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        decode: CatalogRowDecoder<T>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        read.control().check()?;
        if limits.maximum_key_bytes == 0 || limits.maximum_value_bytes == 0 {
            return Err(invalid("zero catalog record limit"));
        }
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
        if live {
            let key = self.after.as_deref().expect("selected record key");
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
