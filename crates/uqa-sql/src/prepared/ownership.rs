//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable preparation metadata keeps its producing allowance through cache snapshots.

use crate::SQLError;
use std::{ops::Deref, sync::Arc};
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug)]
pub struct SharedPreparedInput<T>(Arc<Produced<T>>);

impl<T> SharedPreparedInput<T> {
    /// Admit the shared payload before publication and retain every input constructor's lease.
    pub fn from_produced(
        value: Produced<T>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self(value.into_shared(control)?))
    }

    pub fn reserved_bytes(&self) -> usize {
        self.0.reserved_bytes()
    }
}

impl<T> Clone for SharedPreparedInput<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for SharedPreparedInput<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> From<T> for SharedPreparedInput<T> {
    /// Publish an ordinary analysis input without claiming admission for its earlier allocation.
    fn from(value: T) -> Self {
        let control = ProductionControl::uncontrolled();
        Self::from_produced(
            control.finish(value, None).expect("ordinary input"),
            &control,
        )
        .expect("ordinary sharing")
    }
}

impl<T: Default> Default for SharedPreparedInput<T> {
    fn default() -> Self {
        T::default().into()
    }
}

impl<T: PartialEq> PartialEq for SharedPreparedInput<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Eq> Eq for SharedPreparedInput<T> {}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn metadata_snapshots_share_payload_and_release_it_with_the_last_owner() {
        let source = "x".repeat(4 << 20);
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input =
            SharedPreparedInput::from_produced(control.copy_text(&source).unwrap(), &control)
                .unwrap();
        let retained = budget.used();
        assert_eq!(input.reserved_bytes(), retained);
        let mut snapshot = None;
        let allocation = allocation_counter::measure(|| snapshot = Some(input.clone()));
        let snapshot = snapshot.unwrap();
        assert_eq!(allocation.count_total, 0);
        assert!(std::ptr::eq(input.as_ptr(), snapshot.as_ptr()));
        drop(input);
        assert_eq!(budget.used(), retained);
        assert_eq!(&**snapshot, source);
        drop(snapshot);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn failed_metadata_publication_releases_the_unpublished_input() {
        let source = "x".repeat(1024);
        let budget = MemoryBudget::new(source.len());
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let value = control.copy_text(&source).unwrap();
        assert_eq!(budget.used(), source.len());
        let error = SharedPreparedInput::from_produced(value, &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
    }
}
