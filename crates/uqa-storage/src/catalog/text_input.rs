//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog text loading retains one provider view and its next record slot.

use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedString, BudgetedVec};

#[derive(Clone, Copy, Debug)]
pub enum CatalogTextFamily {
    Metadata,
    Models,
    ScoringParameters,
    Analyzers,
    AnalyzerDescriptors,
    PathIndexes,
}

#[derive(Clone, Copy, Debug)]
pub struct CatalogTextLimits {
    pub maximum_key_bytes: usize,
    pub maximum_value_bytes: usize,
}

/// Original stored text; this boundary neither parses nor canonicalizes its payload.
#[derive(Debug)]
pub struct CatalogTextRow {
    pub name: String,
    pub value: String,
}

impl CatalogTextRow {
    /// Admit both original UTF-8 buffers before copying. The returned row retains the actual native capacities independently of the cursor and provider.
    pub fn copy(
        name: &str,
        value: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let mut key = BudgetedString::new(control.memory());
        key.push_str(name)?;
        let mut body = BudgetedString::new(control.memory());
        body.push_str(value)?;
        let (name, mut memory) = key.into_parts();
        let (value, body_memory) = body.into_parts();
        memory.absorb(body_memory);
        control.check()?;
        Ok(Budgeted::new(Self { name, value }, memory))
    }
}

/// A tombstone or a provider-owned absent text field consumes one slot without returning a row.
#[derive(Debug)]
pub enum CatalogTextStep {
    Pending,
    Row(Budgeted<CatalogTextRow>),
    Complete,
}

/// Decode the selected physical representation with its existing catalog predicates. The callback cannot reenter persistence or invoke user code.
pub type CatalogTextDecoder =
    fn(&[u8], &[u8], &StorageReadControl) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>>;

pub struct CatalogTextInput {
    state: State,
}

enum State {
    Reading(Budgeted<Box<Input>>),
    Complete,
    Failed,
}

struct Input {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    prefix: BudgetedVec<u8>,
    after: Option<BudgetedVec<u8>>,
    decode: CatalogTextDecoder,
    limits: CatalogTextLimits,
    control: StorageReadControl,
}

impl CatalogTextInput {
    /// Retain an already selected native view without enumerating records. Versioned providers supply the same committed/private view to key selection and bounded value reads. Physical key order is preserved; consumers that require name order retain and order the complete decoded catalog in their owning layer.
    pub fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        prefix: &[u8],
        decode: CatalogTextDecoder,
        limits: CatalogTextLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        read.control().check()?;
        if limits.maximum_key_bytes == 0 || limits.maximum_value_bytes == 0 {
            return Err(invalid("zero catalog record limit"));
        }
        control.check_value_size(prefix.len(), limits.maximum_key_bytes)?;
        let memory = control.memory().reserve(size_of::<Input>())?;
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
    pub fn advance(&mut self) -> StorageBackendResult<CatalogTextStep> {
        match self.state {
            State::Complete => return Ok(CatalogTextStep::Complete),
            State::Failed => return Err(invalid("catalog text input is finished")),
            State::Reading(_) => (),
        }
        let State::Reading(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        let step = input.as_mut().advance()?;
        self.state = if matches!(step, CatalogTextStep::Complete) {
            drop(input);
            State::Complete
        } else {
            State::Reading(input)
        };
        Ok(step)
    }
}

impl Input {
    fn advance(&mut self) -> StorageBackendResult<CatalogTextStep> {
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
            return Ok(CatalogTextStep::Complete);
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
        Ok(row.map_or(CatalogTextStep::Pending, CatalogTextStep::Row))
    }
}

fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
