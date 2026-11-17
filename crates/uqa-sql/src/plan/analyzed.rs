//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable source topology retains the reservations of its independently shared analysis generation.

use super::AnalyzedSource;
use crate::SQLError;
use std::{ops::Deref, sync::Arc};
use uqa_core::memory::{PairedRetirementNode, Produced, ProductionControl, TreeRetirement};

#[derive(Clone, Debug)]
pub struct SharedAnalyzedSource(Storage);

#[derive(Clone, Debug)]
enum Storage {
    Ordinary(Arc<AnalyzedSource>),
    Admitted(Arc<Produced<AnalyzedSource>>),
}

impl SharedAnalyzedSource {
    pub fn shares_storage(&self, other: &Self) -> bool {
        std::ptr::eq::<AnalyzedSource>(&raw const **self, &raw const **other)
    }
    pub fn from_produced(
        value: Produced<AnalyzedSource>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(Storage::Admitted(value.into_shared(control)?)))
    }

    pub fn reserved_bytes(&self) -> usize {
        match &self.0 {
            Storage::Ordinary(_) => 0,
            Storage::Admitted(value) => value.reserved_bytes(),
        }
    }
}

impl Deref for SharedAnalyzedSource {
    type Target = AnalyzedSource;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Storage::Ordinary(value) => value,
            Storage::Admitted(value) => value,
        }
    }
}

impl From<Arc<AnalyzedSource>> for SharedAnalyzedSource {
    fn from(value: Arc<AnalyzedSource>) -> Self {
        Self(Storage::Ordinary(value))
    }
}

impl PairedRetirementNode for AnalyzedSource {
    type Local = ();
    fn children_mut(&mut self) -> &mut Option<Box<(Self, Self)>> {
        &mut self.children
    }
    fn retire_local(&mut self, (): &mut ()) -> bool {
        // Shared row descriptors keep their existing owner; only topology retirement is driven here.
        true
    }
}

impl Drop for AnalyzedSource {
    fn drop(&mut self) {
        if self.children.is_some() {
            drop(TreeRetirement::take(&mut self.children));
        }
    }
}
