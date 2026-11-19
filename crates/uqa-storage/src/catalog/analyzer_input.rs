//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field bindings and compatibility labels retain one selected physical catalog.

use super::input::{CatalogInput, CatalogInputLimits, CatalogInputStep, CatalogRowDecoder};
use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedString};

mod ordering;

#[derive(Debug, PartialEq, Eq)]
pub struct CatalogAnalyzerBindingRow {
    pub table: String,
    pub field: String,
    pub binding_json: String,
}

impl CatalogAnalyzerBindingRow {
    /// Admit original text without interpreting or canonicalizing the binding envelope.
    pub fn copy(
        table: &str,
        field: &str,
        binding_json: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        let ([table, field, binding_json], memory) =
            copy([table, field, binding_json], control)?.into_parts();
        Ok(Budgeted::new(
            Self {
                table,
                field,
                binding_json,
            },
            memory,
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CatalogAnalyzerLabelRow {
    pub table: String,
    pub field: String,
    pub phase: String,
    pub analyzer_name: String,
}

impl CatalogAnalyzerLabelRow {
    /// Keep empty names and the stored phase spelling for the original semantic owner.
    pub fn copy(
        table: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        let ([table, field, phase, analyzer_name], memory) =
            copy([table, field, phase, analyzer_name], control)?.into_parts();
        Ok(Budgeted::new(
            Self {
                table,
                field,
                phase,
                analyzer_name,
            },
            memory,
        ))
    }
}

fn copy<const N: usize>(
    fields: [&str; N],
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<[String; N]>> {
    control.check()?;
    let mut owned: [BudgetedString; N] =
        std::array::from_fn(|_| BudgetedString::new(control.memory()));
    for (destination, text) in owned.iter_mut().zip(fields) {
        control.check()?;
        destination.push_str(text)?;
    }
    control.check()?;
    let mut memory = control.memory().empty_reservation();
    let result = owned.map(|field| {
        let (text, payload) = field.into_parts();
        memory.absorb(payload);
        text
    });
    Ok(Budgeted::new(result, memory))
}

#[derive(Debug)]
pub enum CatalogAnalyzerInputStep {
    Pending,
    Binding(Budgeted<CatalogAnalyzerBindingRow>),
    /// Complete physical bindings precede their original ordering and semantic validation. No label has been read when this boundary is returned.
    BindingsComplete,
    Label(Budgeted<CatalogAnalyzerLabelRow>),
    Complete,
}

pub struct CatalogAnalyzerInput {
    state: State,
}

enum State {
    Bindings(
        CatalogInput<CatalogAnalyzerBindingRow>,
        CatalogInput<CatalogAnalyzerLabelRow>,
    ),
    Labels(CatalogInput<CatalogAnalyzerLabelRow>),
    Complete,
    Failed,
}

impl CatalogAnalyzerInput {
    /// Select both physical projections from the same retained view before reading any values. The consumer owns complete-family ordering and semantic checks.
    pub fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        bindings: (&[u8], CatalogRowDecoder<CatalogAnalyzerBindingRow>),
        labels: (&[u8], CatalogRowDecoder<CatalogAnalyzerLabelRow>),
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let bindings = CatalogInput::new(read.clone(), bindings.0, bindings.1, limits, control)?;
        let labels = CatalogInput::new(read, labels.0, labels.1, limits, control)?;
        Ok(Self {
            state: State::Bindings(bindings, labels),
        })
    }

    /// Consume at most one physical slot, including tombstones and absent binding fields. Failure or unwinding closes both streams; completion retires the common view and remains idempotent.
    pub fn advance(&mut self) -> StorageBackendResult<CatalogAnalyzerInputStep> {
        let (state, step) = match std::mem::replace(&mut self.state, State::Failed) {
            State::Bindings(mut bindings, labels) => match bindings.advance()? {
                CatalogInputStep::Pending => (
                    State::Bindings(bindings, labels),
                    CatalogAnalyzerInputStep::Pending,
                ),
                CatalogInputStep::Row(row) => (
                    State::Bindings(bindings, labels),
                    CatalogAnalyzerInputStep::Binding(row),
                ),
                CatalogInputStep::Complete => (
                    State::Labels(labels),
                    CatalogAnalyzerInputStep::BindingsComplete,
                ),
            },
            State::Labels(mut labels) => match labels.advance()? {
                CatalogInputStep::Pending => {
                    (State::Labels(labels), CatalogAnalyzerInputStep::Pending)
                }
                CatalogInputStep::Row(row) => {
                    (State::Labels(labels), CatalogAnalyzerInputStep::Label(row))
                }
                CatalogInputStep::Complete => (State::Complete, CatalogAnalyzerInputStep::Complete),
            },
            State::Complete => (State::Complete, CatalogAnalyzerInputStep::Complete),
            State::Failed => {
                return Err(StorageBackendError::Other(
                    "analyzer catalog input is finished".into(),
                ))
            }
        };
        self.state = state;
        Ok(step)
    }
}

#[cfg(test)]
mod tests;
