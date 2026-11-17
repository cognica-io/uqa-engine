//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared schemas transfer their original fields to SQL's existing field and type retirement state.

pub(in crate::schema) mod fields;
mod index;
pub(in crate::schema) mod maps;

use super::{RowSchema, SchemaIndex};
use crate::plan::retirement::ScalarRetirement;
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct IndexOwner(Option<Arc<SchemaIndex>>);

impl IndexOwner {
    pub(super) fn new(index: SchemaIndex) -> Self {
        Self(Some(Arc::new(index)))
    }
}
impl Deref for IndexOwner {
    type Target = Arc<SchemaIndex>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("live row schema")
    }
}
impl DerefMut for IndexOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().expect("live row schema")
    }
}
impl std::fmt::Debug for IndexOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.deref().fmt(f)
    }
}
impl IndexOwner {
    fn take_retirement(&mut self) -> Retirement {
        Retirement {
            fields: self
                .0
                .take()
                .and_then(Arc::into_inner)
                .map(index::Fields::from),
            state: ScalarRetirement::default(),
        }
    }
}
impl Drop for IndexOwner {
    fn drop(&mut self) {
        drop(self.take_retirement());
    }
}

pub struct Retirement {
    fields: Option<index::Fields>,
    state: ScalarRetirement,
}
impl Retirement {
    pub(crate) fn advance(&mut self) -> bool {
        if self.state.advance(&mut self.fields) {
            self.fields = None;
            true
        } else {
            false
        }
    }
}
impl Drop for Retirement {
    fn drop(&mut self) {
        while !self.advance() {}
    }
}
impl RowSchema {
    /// Consuming cleanup detaches the last owned generation; surviving readers keep its original fields and lease.
    pub(crate) fn take_retirement(&mut self) -> Retirement {
        self.index.take_retirement()
    }

    pub(crate) async fn retire_from<W: crate::catalog::work::Work>(
        mut self,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), crate::SQLError> {
        let mut retirement = self.take_retirement();
        loop {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
            if retirement.advance() {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
