//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named catalog text retains the original stored UTF-8 without interpreting it.

use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::{Budgeted, BudgetedString};

pub use super::input::CatalogInputLimits as CatalogTextLimits;
pub type CatalogTextInput = super::input::CatalogInput<CatalogTextRow>;
pub type CatalogTextStep = super::input::CatalogInputStep<CatalogTextRow>;
pub type CatalogTextDecoder = super::input::CatalogRowDecoder<CatalogTextRow>;

#[derive(Clone, Copy, Debug)]
pub enum CatalogTextFamily {
    Metadata,
    Models,
    ScoringParameters,
    Analyzers,
    AnalyzerDescriptors,
    PathIndexes,
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

#[cfg(test)]
mod tests;
