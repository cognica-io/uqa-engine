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

mod retirement;

#[derive(Clone)]
pub struct SharedAnalyzedSource(Option<Storage>);

impl std::fmt::Debug for SharedAnalyzedSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SharedAnalyzedSource")
            .field(self.0.as_ref().expect("live analyzed source"))
            .finish()
    }
}

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
        Ok(Self(Some(Storage::Admitted(value.into_shared(control)?))))
    }

    pub fn reserved_bytes(&self) -> usize {
        match self.0.as_ref().expect("live analyzed source") {
            Storage::Ordinary(_) => 0,
            Storage::Admitted(value) => value.reserved_bytes(),
        }
    }
}

impl Deref for SharedAnalyzedSource {
    type Target = AnalyzedSource;
    fn deref(&self) -> &Self::Target {
        match self.0.as_ref().expect("live analyzed source") {
            Storage::Ordinary(value) => value,
            Storage::Admitted(value) => value,
        }
    }
}

impl From<Arc<AnalyzedSource>> for SharedAnalyzedSource {
    fn from(value: Arc<AnalyzedSource>) -> Self {
        Self(Some(Storage::Ordinary(value)))
    }
}

impl PairedRetirementNode for AnalyzedSource {
    type Local = Option<crate::schema::retirement::Retirement>;
    fn children_mut(&mut self) -> &mut Option<Box<(Self, Self)>> {
        &mut self.children
    }
    fn retire_local(&mut self, local: &mut Self::Local) -> bool {
        let retirement = local.get_or_insert_with(|| self.schema.take_retirement());
        if retirement.advance() {
            *local = None;
            true
        } else {
            false
        }
    }
}

impl Drop for AnalyzedSource {
    fn drop(&mut self) {
        if self.children.is_some() {
            drop(TreeRetirement::take(&mut self.children));
        }
    }
}
